// Monaco Editor on W3C OS — VS Code's editor core compiled to a native app.
// Entry for the w3cos ESM→Rust AOT pipeline. See docs/monaco-gap-report.md.

import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import "./native.css";

// The pipeline runs the entry module's exported `main()` (esm_codegen run_entry).
export function main() {
    const body = document.body;
    body.setAttribute("data-step", "1-entry");

    const container = document.createElement("div");
    container.setAttribute("id", "editor-host");
    // Monaco measures its host during construction. Give the standalone
    // native example an explicit viewport instead of relying on browser-page
    // CSS (there is no surrounding HTML document stylesheet here).
    container.style.width = "900px";
    container.style.height = "600px";
    body.appendChild(container);
    body.setAttribute("data-step", "2-container");

    const model = monaco.editor.createModel(
        "// Monaco Editor running natively on W3C OS\nfunction hello() {\n  return 42;\n}\n",
        "plaintext"
    );
    body.setAttribute("data-typeof-monaco", typeof monaco);
    body.setAttribute("data-keys-monaco", String(Object.keys(monaco).length));
    body.setAttribute("data-typeof-editor", typeof monaco.editor);
    body.setAttribute("data-keys-editor", String(Object.keys(monaco.editor).length));
    body.setAttribute("data-typeof-createmodel", typeof monaco.editor.createModel);
    body.setAttribute("data-typeof-model", typeof model);
    body.setAttribute("data-model-ownkeys", String(Object.keys(model).length));
    body.setAttribute("data-model-lines", String(model.getLineCount()));
    body.setAttribute("data-model-value", model.getValue());
    model.onDidChangeContent(() => {
        body.setAttribute("data-model-value", model.getValue());
        body.setAttribute("data-model-lines", String(model.getLineCount()));
    });
    body.setAttribute("data-step", "3-model");

    try {
        const editor = monaco.editor.create(container, {
            model: model,
            lineNumbers: "on",
            fontSize: 14,
            lineHeight: 20,
            fontFamily: "monospace",
            theme: "vs-dark",
            // The host has a fixed native viewport; polling ResizeObserver on
            // every animation frame only creates needless continuous redraws.
            automaticLayout: false,
            // The native DOM bridge does not have a browser layout pass
            // available before Monaco's constructor performs its first
            // measurement, so seed the initial editor viewport explicitly.
            dimension: { width: 900, height: 600 },
        });
        body.setAttribute("data-step", "4-editor-created");
        body.setAttribute("data-editor-lines", String(editor.getModel().getLineCount()));
        body.setAttribute("data-step", "5-done");
    } catch (err) {
        body.setAttribute("data-step", "4-create-threw");
        body.setAttribute("data-error", String(err));
    }
}
