<script setup lang="ts">
import { NConfigProvider, NMessageProvider, NLayout, NLayoutHeader, NLayoutContent, NH1, NP } from "naive-ui";
</script>

<template>
  <n-config-provider>
    <n-message-provider>
      <n-layout style="height: 100vh">
        <n-layout-header bordered style="padding: 16px">
          <n-h1 style="margin: 0">CC Router</n-h1>
        </n-layout-header>
        <n-layout-content style="padding: 16px">
          <n-p>骨架就绪。网关内核见实施计划 Task 2 起。</n-p>
        </n-layout-content>
      </n-layout>
    </n-message-provider>
  </n-config-provider>
</template>
