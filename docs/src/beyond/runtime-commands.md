# Running code and tests

Run a saved script, evaluate a selected cell, or test a package through explicit
language-server commands. These commands require `M2` on `PATH` and run only
when you invoke them. Ordinary analysis does not execute your code.

Send `workspace/executeCommand` with one object in `arguments`. For example,
to run the tests for a package from its source checkout:

```json
{
  "command": "m2-ls.runTests",
  "arguments": [{
    "uri": "file:///home/user/projects/MyPackage/MyPackage.m2",
    "package": "MyPackage"
  }]
}
```

`m2-ls.executeFile`
: Requires `uri`, a local file URI. Executes the saved file with M2's `input`.
  Save your buffer first: unsaved edits are not included.

`m2-ls.executeCell`
: Requires `uri` and `code`, the cell source supplied by the caller. The server
  evaluates that text; it does not select a cell from a cursor position.

`m2-ls.runTests`
: Requires `uri` and `package`. Loads the named package from that source file
  and invokes `check` with verbose failure output.

Each command starts a fresh process in the source file's parent directory,
using normal M2 initialization. Definitions do not persist between invocations.
The response contains `success`, `exitCode`, `stdout`, and `stderr`; output is
returned when the process finishes rather than streamed. A missing executable
or failure to launch is returned as an LSP error. A failed M2 process returns
its captured output and `success: false`.

Clients decide how to present results and bind commands. The server does not
configure editor keymaps or open output windows. Package installation,
documentation generation, and runtime metadata queries are not implemented.
