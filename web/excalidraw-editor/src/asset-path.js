// Runs before the editor bundle: Excalidraw loads its fonts from
// EXCALIDRAW_ASSET_PATH, which must be this session's own folder on the
// loopback server. Without it the package falls back to a public CDN, which
// the page's Content-Security-Policy blocks.
window.EXCALIDRAW_ASSET_PATH = new URL("./", window.location.href).toString();
