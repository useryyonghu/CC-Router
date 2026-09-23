/**
 * 复制到剪贴板。
 *
 * 两个页面都要"点一下复制路径"（设置页的文件位置、子 Agent 页的备份路径），所以抽到这里，
 * 避免同一段逻辑写两遍——本项目已经因为"同一段顺序抄两份"出过一次事故（见 tray.rs 的注释）。
 *
 * 诚实性：两条路都要**如实反馈**。以前兜底路径忽略 `document.execCommand("copy")` 的布尔返回值，
 * 失败了照样弹"已复制"。
 */
import { useMessage } from "naive-ui";
import type { MessageApi } from "naive-ui";
import { errorText } from "../api/ipc";

export interface CopyHelper {
  copyText: (text: string, label: string) => void;
}

export function useCopy(): CopyHelper {
  const message: MessageApi = useMessage();

  function copyText(text: string, label: string): void {
    if (!text) {
      message.warning(`${label}为空`);
      return;
    }
    const fallback = (): boolean => {
      const area = document.createElement("textarea");
      area.value = text;
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.appendChild(area);
      area.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(area);
      return ok;
    };
    const fallbackThen = (): void => {
      if (fallback()) message.success(`${label}已复制`);
      else message.error(`${label}复制失败：浏览器拒绝了复制操作，请手动选中后按 Ctrl+C`);
    };
    try {
      if (navigator.clipboard?.writeText) {
        navigator.clipboard.writeText(text).then(
          () => message.success(`${label}已复制`),
          () => fallbackThen(),
        );
      } else {
        fallbackThen();
      }
    } catch (err) {
      message.error(`复制失败：${errorText(err)}`);
    }
  }

  return { copyText };
}
