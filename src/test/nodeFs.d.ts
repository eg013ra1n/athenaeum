// Delete this file when `@types/node` becomes a devDependency: its declarations would clash with these.
// The few Node facilities the truth-table tests read the shared Rust fixture with. `@types/node` is not a
// dependency of the app, so they are declared here, at exactly the shape the tests use.
declare module 'node:fs' {
  export function readFileSync(path: string, encoding: 'utf8'): string;
}
declare module 'node:path' {
  export function resolve(...parts: string[]): string;
}
declare const __dirname: string;
