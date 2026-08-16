// Ambient declarations for CSS side-effect imports (`import "./x.css"`).
// Vite handles these at build; tsgo (TS7) requires the declaration under
// moduleResolution:bundler.
declare module "*.css";
