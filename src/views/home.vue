<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";

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
  <div>
    <h1>首页</h1>
    <main>
      <button @click="setWallpaper">设置壁纸</button>
      <button @click="removeWallpaper">移除壁纸</button>
    </main>
  </div>
</template>
