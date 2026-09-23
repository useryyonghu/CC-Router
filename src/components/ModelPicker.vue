<script setup lang="ts">
/**
 * B4/B5：`provider / 模型` 两级下拉（复用）。
 *
 * 值用 `TargetDto`（`{ providerId, modelId }`），选项来自 store 里**全部已配置模型**
 * （spec §10.3）。当前值若已不在配置里（模型被删/改名后的旧绑定），会额外渲染一条
 * 「不在当前配置中」的选项，避免下拉显示空白让人以为没绑定。
 */
import { computed } from "vue";
import { NSelect } from "naive-ui";
import type { SelectGroupOption, SelectOption } from "naive-ui";
import type { TargetDto } from "../api/ipc";
import { useConfigStore } from "../stores/config";

const props = withDefaults(
  defineProps<{
    modelValue: TargetDto | null;
    placeholder?: string;
    disabled?: boolean;
    clearable?: boolean;
    size?: "tiny" | "small" | "medium" | "large";
  }>(),
  {
    placeholder: "选择模型",
    disabled: false,
    clearable: true,
    size: "small",
  },
);

const emit = defineEmits<{ "update:modelValue": [TargetDto | null] }>();

const store = useConfigStore();
/** providerId / modelId 都不允许含控制字符，故用它做分隔符比 `/` 安全（模型名可能含 `/`）。 */
const SEP = "\u241f";

function encode(providerId: string, modelId: string): string {
  return `${providerId}${SEP}${modelId}`;
}

const options = computed<Array<SelectGroupOption | SelectOption>>(() => {
  const groups: SelectGroupOption[] = store.providers
    .filter((provider) => provider.models.length > 0)
    .map((provider) => ({
      type: "group",
      key: provider.id,
      label: `${provider.name}（${provider.id}）`,
      children: provider.models.map((model) => ({
        label: model.name && model.name !== model.id ? `${model.name} · ${model.id}` : model.id,
        value: encode(provider.id, model.id),
      })),
    }));
  const current = props.modelValue;
  if (current && !store.modelSpecOf(current.providerId, current.modelId)) {
    groups.push({
      type: "group",
      key: "__missing__",
      label: "不在当前配置中",
      children: [{ label: `${current.providerId} / ${current.modelId}（已不在配置中）`, value: encode(current.providerId, current.modelId) }],
    });
  }
  return groups;
});

const value = computed<string | null>(() =>
  props.modelValue ? encode(props.modelValue.providerId, props.modelValue.modelId) : null,
);

const resolvedPlaceholder = computed(() =>
  store.models.length === 0 ? "还没有已配置的模型" : props.placeholder,
);

function onChange(raw: string | number | null): void {
  if (raw === null || raw === "") {
    emit("update:modelValue", null);
    return;
  }
  const text = String(raw);
  const index = text.indexOf(SEP);
  if (index < 0) {
    emit("update:modelValue", null);
    return;
  }
  emit("update:modelValue", {
    providerId: text.slice(0, index),
    modelId: text.slice(index + SEP.length),
  });
}
</script>

<template>
  <n-select
    :value="value"
    :options="options"
    :placeholder="resolvedPlaceholder"
    :disabled="disabled || store.models.length === 0"
    :clearable="clearable"
    :size="size"
    filterable
    @update:value="onChange"
  />
</template>
