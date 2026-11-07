import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import WorkerWindow from "./WorkerWindow";
import { AppProvider } from "./store";
// 앱 안에 넣은 폰트(OFL-1.1). 인터넷 없이도 같은 모양으로 보인다.
import "pretendard/dist/web/variable/pretendardvariable-dynamic-subset.css";
import "@fontsource-variable/orbitron";
import "@fontsource-variable/jetbrains-mono";
import "./styles.css";
import "./elysium.css";
import { applyTheme, loadTheme, watchStorage } from "./lib/theme";
import { applyView } from "./lib/view";
import { initLowSpec } from "./lib/perf";

// 첫 화면이 그려지기 전에 테마를 정해 깜빡임을 막는다.
applyTheme(loadTheme());
watchStorage();
// 화면 배율·대화 폭(워커 창도 같은 배율로)
applyView();
initLowSpec();

const query = new URLSearchParams(window.location.search);
const workerTask = query.get("window") === "worker" ? query.get("task") : null;

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {workerTask ? (
      <WorkerWindow task={workerTask} windowKey={query.get("key") ?? ""} />
    ) : (
      <AppProvider>
        <App />
      </AppProvider>
    )}
  </React.StrictMode>,
);
