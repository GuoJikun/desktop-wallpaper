<script setup lang="ts">
import LayoutHeader from "@/components/layout-header.vue";
</script>

<template>
    <div class="page-main">
        <LayoutHeader />
        <main class="page-main-wrap">
            <router-view />
        </main>
    </div>
</template>

<style lang="css">
.layout-main {
    height: 100vh;
    overflow: hidden;
    --window-title: 28px;
}
.page-main-wrap {
    height: calc(100vh - var(--window-title));
    position: relative;
    overflow: hidden auto;
}
</style>
