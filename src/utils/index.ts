import { invoke } from "@tauri-apps/api/core";

export const setWallpaper = async (windowLabel: string) => {
    try {
        await invoke("set_wallpaper", { windowLabel });
    } catch (error) {
        console.error(error);
    }
};

export const removeWallpaper = async (windowLabel: string) => {
    try {
        await invoke("remove_wallpaper", { windowLabel });
    } catch (error) {
        console.error(error);
    }
};

