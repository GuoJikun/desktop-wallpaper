import { invoke } from "@tauri-apps/api/core";
import { getAllWindows } from "@tauri-apps/api/window";

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

export const getWindowByLabel = async (label: string) => {
    const windowList = await getAllWindows();
    return windowList.find(window => window.label === label);
};
