<script setup lang="ts">
import { zhCN, dateZhCN } from "naive-ui";
</script>

<template>
    <n-config-provider :locale="zhCN" :date-locale="dateZhCN">
        <router-view />
    </n-config-provider>
</template>

<style scoped>
html,
body {
    margin: 0;
    padding: 0;
    overflow: hidden;
    height: 100vh;
    width: 100vw;
    font: 14px/1.5 "Helvetica Neue", Arial, "Hiragino Sans GB", "Heiti SC", "Microsoft YaHei", "WenQuanYi Micro Hei",
        sans-serif;
}

#app {
    height: 100%;
    width: 100%;
    overflow: hidden;
    margin: 0;
    padding: 0;
}
</style>
