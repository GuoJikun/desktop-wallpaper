<script setup lang="ts">
import { Settings16Regular as Settings } from "@vicons/fluent";
</script>

<template>
    <Layout class="home">
        <div class="home-header">
            <div class="utils">
                <div class="utils-item">
                    <n-button>更新壁纸列表</n-button>
                </div>
                <div class="utils-item"></div>
                <div class="utils-item">
                    <router-link to="/test">测试页面</router-link>
                </div>
            </div>
            <div class="extra">
                <div class="extra-item">
                    <router-link to="/settings">
                        <n-icon size="20"><Settings /></n-icon>
                    </router-link>
                </div>
            </div>
        </div>
        <div class="content"></div>
        <div class="footer"></div>
    </Layout>
</template>
<style lang="scss" scoped>
.home {
    &-header {
        display: flex;
        flex-direction: row;
        align-items: center;
        justify-content: space-between;
        gap: 24px;
        .utils {
            display: flex;
            align-items: center;
            gap: 16px;
        }
    }
}
</style>
