import { app, BrowserWindow } from "electron";
import { readFileSync } from "fs";

export function open() {
    const window = new BrowserWindow({});
    app.whenReady();
    readFileSync("example.ts");
    eval("1");
}
