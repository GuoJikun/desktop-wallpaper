import { createRouter, createWebHistory } from 'vue-router';
import Home from '../views/home.vue';
import Wallpager from '../views/wallpaper.vue';

const router = createRouter({
    history: createWebHistory(),
    routes: [
        { path: '/', component: Home },
        { path: '/wallpaper', component: Wallpager },
    ],
});

export default router;