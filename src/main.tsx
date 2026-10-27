import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { AppProvider } from "./store";
// 앱 안에 넣은 폰트(OFL-1.1). 인터넷 없이도 같은 모양으로 보인다.
import "pretendard/dist/web/variable/pretendardvariable-dynamic-subset.css";
import "@fontsource-variable/orbitron";
import "@fontsource-variable/jetbrains-mono";
import "./styles.css";
import { applyTheme, loadTheme } from "./lib/theme";

// 첫 화면이 그려지기 전에 테마를 정해 깜빡임을 막는다.
applyTheme(loadTheme());

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <AppProvider>
      <App />
    </AppProvider>
  </React.StrictMode>,
);
