export interface Wallpaper {
    /*唯一标识*/
    id: string;
    // 显示的名字
    name: string;
    // 描述
    description: string;
    repo: string;
    type: "image" | "html";
    homeUrl: string;
    // 封面图
    image: string | Array<string>;
}

export type WallpaperList = Array<Wallpaper>;
