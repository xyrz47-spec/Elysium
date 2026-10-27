import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Android 개발 실행 때는 폰이 PC의 개발 서버에 붙도록 TAURI_DEV_HOST에서 연다.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  // 이 폴더(mobile)에서 실행한다(npm run dev:mobile / build:mobile).
  plugins: [react()],
  clearScreen: false,
  build: { outDir: "../dist-mobile", emptyOutDir: true },
  server: {
    port: 1430,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1431 } : undefined,
    // PC 앱과 같이 쓰는 컴포넌트·색(../src)을 읽을 수 있게 한다.
    fs: { allow: [".."] },
  },
});
