<script setup lang="ts">
import LayoutHeader from "@/components/layout-header.vue";
</script>

<template>
    <div class="layout-main">
        <LayoutHeader />
        <main class="layout-main-wrap">
            <slot />
        </main>
    </div>
</template>

<style lang="scss" scoped>
.layout-main {
    height: 100vh;
    overflow: hidden;
    --window-title: 28px;
    &-wrap {
        height: calc(100vh - var(--window-title));
        position: relative;
        overflow: hidden auto;
    }
}
</style>
