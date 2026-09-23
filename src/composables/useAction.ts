/**
 * 「按钮 → IPC」的统一执行器。
 *
 * 存在的理由：后端所有失败都是 `Err(String)`（中文、面向用户、常常很长，
 * 例如「一键获取模型」失败时会列出每个候选 URL 的尝试结果）。spec §5.7 要求
 * **不允许静默失败**，所以这里统一把它们弹成可关闭的、停留 8 秒的错误提示。
 */
import { computed, ref } from "vue";
import type { ComputedRef, Ref } from "vue";
import { useMessage } from "naive-ui";
import type { MessageApi } from "naive-ui";
import { errorText } from "../api/ipc";

export interface ActionRunner {
  /** 正在执行的动作的 key（`""` 表示通用动作）；用于按钮 loading / 行内禁用。 */
  busyKey: Ref<string | null>;
  busy: ComputedRef<boolean>;
  isBusy: (key?: string) => boolean;
  run: (action: () => Promise<unknown>, okText?: string, key?: string) => Promise<boolean>;
  message: MessageApi;
}

export function useAction(): ActionRunner {
  const message = useMessage();
  const busyKey = ref<string | null>(null);
  const busy = computed(() => busyKey.value !== null);

  function isBusy(key = ""): boolean {
    return busyKey.value === key;
  }

  async function run(action: () => Promise<unknown>, okText?: string, key = ""): Promise<boolean> {
    busyKey.value = key;
    try {
      await action();
      if (okText) message.success(okText);
      return true;
    } catch (err) {
      message.error(errorText(err), { duration: 8000, closable: true, keepAliveOnHover: true });
      return false;
    } finally {
      busyKey.value = null;
    }
  }

  return { busyKey, busy, isBusy, run, message };
}
