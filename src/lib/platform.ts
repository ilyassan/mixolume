// `navigator.platform` is deprecated for web content but reliable here: this only ever runs
// inside Tauri's own bundled webview (WKWebView/WebView2/WebKitGTK), never an arbitrary browser.
export const isMac = navigator.platform.toUpperCase().includes("MAC");
