//! 单实例守卫：**第二次启动不再开第二个窗口，而是把已有窗口拿到前台**。
//!
//! 起因（用户反馈）：从开始菜单/桌面图标再点一次，会又起一个进程、又开一个窗口。
//! 两个实例会各自持有一份网关状态与接管状态，用户看到的是"两个 CC Router"。
//!
//! # 为什么不用 `tauri-plugin-single-instance`
//!
//! 那需要新增一个 Cargo 依赖；本项目的硬约束是"不新增依赖"，而且本机能否联网取 crate
//! 也未必（当前依赖树是缓存里就有的）。这里用标准库实现，效果等价：
//!
//! 1. 启动时尝试 `bind(127.0.0.1:<PORT>)`。绑得上 ⇒ 我是第一个实例，持有这个 listener。
//! 2. 绑不上 ⇒ 可能已有实例，也可能是别的程序占了端口。**用口令握手区分**：
//!    连上去写魔术串，写成功就认为"已有实例"（并让它把窗口拿到前台），然后本进程直接退出、
//!    连窗口都不建。
//! 3. 握手也失败 ⇒ 端口被陌生程序占用。此时**照常启动**（只打一行日志）：
//!    绝不能因为一个端口占用就不让用户用应用。
//!
//! 握手是必要的：只凭"端口被占用"就退出，会让用户在一个恰好占用该端口的机器上永远打不开应用。

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// 单实例守卫用的回环端口。选一个不常见的高位端口，降低与别的程序相撞的概率。
const PORT: u16 = 45_737;

/// 握手口令。末尾带换行便于用 `read_line` 之类的方式读取；内容本身不敏感。
const MAGIC: &[u8] = b"cc-router/single-instance:show-window\n";

/// 首次启动的结果。
pub enum Startup {
    /// 本进程是第一个实例。`listener` **必须一直持有**（存活到进程结束），
    /// 否则端口被释放，后续实例就认不出"已有实例"了。
    Primary(TcpListener),
    /// 已有实例在跑，并且已经通知它把窗口拿到前台。**本进程应立即退出，不要建窗口。**
    Secondary,
    /// 端口被陌生程序占用，无法保证单实例 —— 但必须照常启动（附上原因供日志）。
    Unavailable(String),
}

/// 用默认端口认领单实例身份。
pub fn claim() -> Startup {
    claim_on(PORT)
}

/// 指定端口版本（测试用，避免与真机上正在运行的应用抢同一个端口）。
pub fn claim_on(port: u16) -> Startup {
    match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))) {
        Ok(listener) => Startup::Primary(listener),
        Err(bind_err) => match notify_existing_on(port) {
            Ok(()) => Startup::Secondary,
            Err(why) => Startup::Unavailable(format!("{bind_err}；口令握手也失败：{why}")),
        },
    }
}

/// 通知已有实例把窗口拿到前台。
fn notify_existing_on(port: u16) -> std::io::Result<()> {
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(800),
    )?;
    stream.write_all(MAGIC)?;
    stream.flush()
}

/// 在后台线程上等后续实例的请求：每收到一条**合法**口令就调一次 `on_show`。
///
/// 收到看不懂的连接（别的程序探到该端口、端口扫描等）时**什么都不做** ——
/// 绝不能因为一次陌生连接就把窗口弹出来。
pub fn serve_show_requests<F>(listener: TcpListener, on_show: F) -> std::thread::JoinHandle<()>
where
    F: Fn() + Send + 'static,
{
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buf = [0u8; MAGIC.len()];
            let ok = matches!(stream.read(&mut buf), Ok(n) if n == MAGIC.len() && &buf[..n] == MAGIC);
            if ok {
                on_show();
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 取一个空闲端口：绑 0 让系统分配，读出来再释放给测试用。
    fn free_port() -> u16 {
        let probe = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("取空闲端口");
        probe.local_addr().expect("读端口").port()
    }

    #[test]
    fn first_claim_is_primary_and_second_is_secondary() {
        let port = free_port();
        let first = claim_on(port);
        let Startup::Primary(listener) = first else {
            panic!("首次认领应当是 Primary（端口是刚释放的），实际不是");
        };
        match claim_on(port) {
            Startup::Secondary => {}
            Startup::Primary(_) => panic!("第二次认领不应再拿到 Primary —— 那就会出现两个窗口"),
            Startup::Unavailable(why) => panic!("第二次认领应是 Secondary（本应用在监听），却是 Unavailable：{why}"),
        }
        drop(listener);
    }

    #[test]
    fn valid_handshake_shows_window_and_garbage_does_not() {
        let port = free_port();
        let Startup::Primary(listener) = claim_on(port) else {
            panic!("首次认领应当是 Primary");
        };
        let (tx, rx) = mpsc::channel::<&'static str>();
        let _handle = serve_show_requests(listener, move || {
            let _ = tx.send("show");
        });

        // 陌生连接：写了垃圾内容，必须**不**触发弹窗
        let mut stranger = TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).expect("连接");
        stranger.write_all(b"GET / HTTP/1.0\r\n\r\n").expect("写垃圾");
        drop(stranger);
        assert!(
            rx.recv_timeout(Duration::from_millis(400)).is_err(),
            "陌生连接不该把窗口弹出来"
        );

        // 合法口令：必须触发一次
        notify_existing_on(port).expect("发送口令");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).expect("口令应当触发 on_show"),
            "show"
        );
    }
}
