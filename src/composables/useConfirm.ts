/**
 * 确认对话框（Promise 化）。删除服务商 / 还原接管 / 重新生成令牌等破坏性动作都要过它。
 */
import { useDialog } from "naive-ui";
import type { DialogApi } from "naive-ui";

export interface ConfirmOptions {
  title: string;
  content: string;
  positiveText?: string;
  negativeText?: string;
  /** 破坏性动作用红色确认按钮。 */
  danger?: boolean;
}

export interface ConfirmRunner {
  confirm: (options: ConfirmOptions) => Promise<boolean>;
  dialog: DialogApi;
}

export function useConfirm(): ConfirmRunner {
  const dialog = useDialog();

  function confirm(options: ConfirmOptions): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      let settled = false;
      const done = (value: boolean) => {
        if (settled) return;
        settled = true;
        resolve(value);
      };
      const api = options.danger ? dialog.warning : dialog.info;
      api({
        title: options.title,
        content: options.content,
        positiveText: options.positiveText ?? "确定",
        negativeText: options.negativeText ?? "取消",
        onPositiveClick: () => done(true),
        onNegativeClick: () => done(false),
        onClose: () => done(false),
        onMaskClick: () => done(false),
        onEsc: () => done(false),
      });
    });
  }

  return { confirm, dialog };
}
