import { createRouter, createWebHistory } from "vue-router";
import Main from "@/views/main/layout.vue";
import Home from "@/views/main/home.vue";
import Wallpager from "@/views/wallpaper/index.vue";

const router = createRouter({
    history: createWebHistory(),
    routes: [
        {
            path: "/",
            component: Main,
            redirect: "/home",
            children: [
                { path: "/home", component: Home },
                { path: "/test", component: import("@/views/main/test.vue") },
                { path: "/settings", component: import("@/views/main/settings.vue") },
            ],
        },
        { path: "/wallpaper", component: Wallpager },
    ],
});

export default router;
