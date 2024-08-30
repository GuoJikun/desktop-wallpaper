import { createRouter, createWebHistory } from "vue-router";

import Home from "@/views/main/home.vue";
import Wallpager from "@/views/wallpaper/index.vue";

const routes = [
    { path: "/", component: Home },
    { path: "/test", component: () => import("@/views/main/test.vue") },
    { path: "/settings", component: () => import("@/views/main/settings.vue") },
    { path: "/wallpaper", component: Wallpager },
];

const router = createRouter({
    history: createWebHistory(),
    routes,
});

export default router;
