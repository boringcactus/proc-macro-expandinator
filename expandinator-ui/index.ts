import { EditorState, basicSetup } from "@codemirror/basic-setup";
import { EditorView, keymap } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { rust } from "@codemirror/lang-rust";
import targets from "../out/targets";

let targetWasm: any;

const crateSelect = document.getElementById("crate") as HTMLSelectElement;
crateSelect.innerHTML = "<option value=''>Select a crate</option>";
for (const targetLabel in targets) {
  const thisOption = document.createElement("option");
  thisOption.textContent = targetLabel;
  crateSelect.appendChild(thisOption);
}

const macroSelect = document.getElementById("macro") as HTMLSelectElement;
crateSelect.addEventListener("input", async () => {
  if (crateSelect.value === "") return;
  macroSelect.innerHTML = "<option value=''>Loading macros...</option>";
  const macros = await targets[crateSelect.value].data();
  macroSelect.innerHTML = "<option value=''>Select a macro</option>";
  for (const macroLabel in macros) {
    const macroValue = macros[macroLabel];
    const thisMacro = document.createElement("option");
    thisMacro.textContent = macroLabel;
    thisMacro.value = macroValue;
    macroSelect.appendChild(thisMacro);
  }
  targetWasm = await targets[crateSelect.value].lib();
  targetWasm.default();
});

macroSelect.addEventListener("input", expand);

let inputEditor = new EditorView({
  state: EditorState.create({
    extensions: [
      basicSetup,
      keymap.of([indentWithTab]),
      rust(),
      EditorView.updateListener.of((update) => {
        if (update.docChanged) {
          expand();
        }
      }),
    ],
  }),
  parent: document.getElementById("input")!,
});

let outputEditor = new EditorView({
  state: EditorState.create({
    doc: "// enter input and the macro will be expanded",
    extensions: [basicSetup, rust(), EditorState.readOnly.of(true)],
  }),
  parent: document.getElementById("output")!,
});

function expand() {
  let output;
  if (macroSelect.value === "") {
    output = "// enter input and pick a macro and the macro will be expanded";
  } else {
    const targetFunction = targetWasm[macroSelect.value];
    const input = inputEditor.state.doc.toString();
    output = targetFunction(input);
  }
  outputEditor.dispatch({
    changes: {
      from: 0,
      to: outputEditor.state.doc.length,
      insert: output,
    },
  });
}
