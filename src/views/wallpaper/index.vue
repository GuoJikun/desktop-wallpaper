<script setup lang="ts">
import { onMounted, ref } from "vue";

const sayHello = () => {
    alert("hello world!");
};
const bgc = ref({ backgroundColor: "#fff" });
onMounted(() => {
    window.addEventListener("mousedown", () => {
        bgc.value.backgroundColor = "red";
    });
});
</script>

<template>
    <div class="wallpaper">
        <h1>壁纸</h1>
        <div>
            <n-button @click="sayHello">测试点击功能</n-button>
        </div>
        <div style="height: 100px; width: 200px; background-color: #fff" :style="bgc"></div>
    </div>
</template>

<style lang="scss" scoped>
.wallpaper {
    width: 100vw;
    height: 100vh;
    background-color: beige;
}
</style>
