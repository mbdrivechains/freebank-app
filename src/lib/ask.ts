// Ask FreeBank (v0.4.6): a question to FreeBank's AI helper, asked from Rust (src-tauri/src/ask.rs) at one fixed
// address; the page never talks to the internet. Recovery words are refused before anything is sent.
import { tauriInvoke } from "./api";

export const askHelper = (question: string) => tauriInvoke("ask_helper", { question }) as Promise<string>;
