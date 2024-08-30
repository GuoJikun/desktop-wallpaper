<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { useRouter } from "vue-router";

const router = useRouter();

const handleBack = () => {
    router.back();
};

const setWallpaper = async () => {
    try {
        await invoke("set_wallpaper", { windowLabel: "background" });
    } catch (error) {
        console.error(error);
    }
};

const removeWallpaper = async () => {
    try {
        await invoke("remove_wallpaper", { windowLabel: "background" });
    } catch (error) {
        console.error(error);
    }
};
</script>

<template>
    <Layout>
        <n-page-header @back="handleBack" title="测试页面"></n-page-header>
        <main style="padding: 16px">
            <n-space>
                <n-button @click="setWallpaper">设置壁纸</n-button>
                <n-button @click="removeWallpaper">移除壁纸</n-button>
            </n-space>
        </main>
    </Layout>
</template>
