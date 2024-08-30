<script lang="ts" setup>
import { ref, onMounted } from "vue";
import { app } from "@tauri-apps/api";
import { useRouter } from "vue-router";

const router = useRouter();

const handleBack = () => {
    router.push("/");
};

const getVersion = async () => {
    const version = await app.getVersion();
    return version;
};

const version = ref("0");

onMounted(async () => {
    version.value = await getVersion();
});
</script>

<template>
    <div class="settings">
        <div class="settings-header">
            <n-page-header @back="handleBack" title="设置"></n-page-header>
        </div>
        <div class="settings-anchor">
            <n-anchor style="z-index: 1" :bound="24" type="rail">
                <n-anchor-link title="常规" href="#basic" />
                <n-anchor-link title="下载目录" href="#download" />
                <n-anchor-link title="快捷键" href="#shortcut" />
                <n-anchor-link title="版本" href="#version" />
            </n-anchor>
        </div>
        <div class="settings-body">
            <div class="settings-content">
                <div id="download" class="settings-item">
                    <div class="settings-item-header">
                        <span class="settings-item-title">常规：</span>
                    </div>
                    <div class="settings-item-content">
                        <div>
                            <span>开机自动启动：</span>
                            <n-switch>开机自动启动</n-switch>
                        </div>
                        <div>
                            <span>版本：</span>
                            <span>{{ version }}</span>
                        </div>
                    </div>
                </div>
                <div id="download" class="settings-item">
                    <div class="settings-item-header">
                        <span class="settings-item-title">下载目录：</span>
                    </div>
                    <div class="settings-item-content">
                        <div>
                            <span>版本：</span>
                            <span>{{ version }}</span>
                        </div>
                    </div>
                </div>
                <div id="shortcut" class="settings-item">
                    <div class="settings-item-header">
                        <span class="settings-item-title">快捷键：</span>
                    </div>
                    <div class="settings-item-content">
                        <div>
                            <span>版本：</span>
                            <span>{{ version }}</span>
                        </div>
                    </div>
                </div>
                <div id="version" class="settings-item">
                    <div class="settings-item-header">
                        <span class="settings-item-title">关于：</span>
                    </div>
                    <div class="settings-item-content">
                        <div>
                            <span>版本：</span>
                            <span>{{ version }}</span>
                        </div>
                    </div>
                </div>
            </div>
        </div>
    </div>
</template>

<style lang="scss" scoped>
.settings {
    --settings-header-height: 40px;
    --settings-aside-width: 200px;
    min-height: 100%;
    &-header {
        position: sticky;
        top: var(--window-title);
        left: 0;
        background-color: #fff;
        z-index: 1000;
        height: var(--settings-header-height);
        border-bottom: 1px solid #f1f1f1;
        display: flex;
        align-items: center;
    }
    &-body {
        display: flex;
        flex-direction: row;
        align-items: flex-start;
        padding: 24px 24px 0 var(--settings-aside-width);
    }

    &-anchor {
        position: fixed;
        top: calc(--settings-header-height + var(--window-title));
        left: 0;
        height: 100vh;
        overflow: auto;
        width: 0 0 var(--settings-aside-width);
        padding-left: 12px;
        padding-top: 12px;
    }
    &-body {
        flex: 0 0 calc(100% - 200px);
    }

    &-item {
        color: #666666;
        & + & {
            margin-top: 12px;
        }
        &-header {
            color: #333333;
            font-size: 14px;
            line-height: 2em;
        }
        &-content {
            padding: 8px 12px;
            display: flex;
            flex-direction: column;
            gap: 8px;
        }
    }
}
</style>
