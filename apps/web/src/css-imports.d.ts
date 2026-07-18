// Ambient declarations for CSS side-effect imports (`import "./x.css"`).
// Vite handles these at build; tsc 5.9 was lenient under moduleResolution:bundler,
// tsgo (TS7) requires the declaration. Covers local *.css + the @wterm/dom/css subpath.
declare module "*.css";
declare module "@wterm/dom/css";
