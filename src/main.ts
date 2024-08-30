import { createApp } from "vue";
import App from "./App.vue";
import router from "./router";

import naive from 'naive-ui'
// 通用字体
import 'vfonts/Lato.css'
// 等宽字体
import 'vfonts/FiraCode.css'

import "./assets/reset.css";

const app = createApp(App)
app.use(router)
app.use(naive)
app.mount("#app");
