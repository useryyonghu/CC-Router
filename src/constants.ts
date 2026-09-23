/**
 * 关于页 / 全局文案里用到的常量。
 *
 * `CC_SWITCH_LICENSE_TEXT` 是 `docs/reference/cc-switch-LICENSE.txt` 的逐字副本：
 * spec §5.6 要求应用「关于」页保留来源署名与 MIT 许可证全文。
 */

/** 五个页面（B1 用本地 `ref` 切页，不引 vue-router）。 */
export type PageKey = "status" | "providers" | "roles" | "agents" | "settings";

/**
 * 应用版本兜底值，与 `src-tauri/tauri.conf.json` 的 `version` 一致。
 * Plan 3 §A4 没有提供读取版本的 IPC 命令；`getAppVersion()` 优先用
 * `@tauri-apps/api/app` 的 `getVersion()`，失败时才退回这里。
 */
export const APP_VERSION_FALLBACK = "0.1.0";

/** 预设数据来源（spec §5.6 要求署名）。 */
export const CC_SWITCH_SOURCE = "farion1231/cc-switch";
export const CC_SWITCH_SOURCE_REF = "main";
export const CC_SWITCH_URL = "https://github.com/farion1231/cc-switch";

/** MIT 许可证全文（逐字来自 `docs/reference/cc-switch-LICENSE.txt`）。 */
export const CC_SWITCH_LICENSE_TEXT = `MIT License

Copyright (c) 2025 Jason Young

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
`;
