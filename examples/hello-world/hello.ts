// Every host-callable must be declared explicitly — tscc has no ambient I/O
// and `console.log` is not a built-in. The host in `main.rs` satisfies this
// import with a wasmtime linker entry; see the example's README for why.
declare function log(s: string): void;

export function main(): void {
    log("hello from tscc!");
    log("this string crossed the WASM boundary via a declared host import.");

    const n: i32 = 7;
    log("seven squared is " + (n * n).toString());
}
