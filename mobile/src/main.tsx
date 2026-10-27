import React from "react";
import ReactDOM from "react-dom/client";
import "pretendard/dist/web/variable/pretendardvariable-dynamic-subset.css";
import "@fontsource-variable/orbitron";
import "@fontsource-variable/jetbrains-mono";
import "../../src/tokens.css";
import "./mobile.css";
import { applyTheme, loadTheme } from "../../src/lib/theme";
import App from "./App";

applyTheme(loadTheme());

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
