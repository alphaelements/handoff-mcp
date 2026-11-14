## Documentation Contract

Source-of-truth documents and when to update them.

- Document map (path or glob, and the document that describes it): TODO(handoff)
- Generated documents and the command that regenerates each: TODO(handoff)

Rules:

- Update a normative spec only when there is evidence of an intended contract
  change (a task requirement, a user instruction, or an approved decision).
- A contradiction between code and spec without such evidence is a code finding,
  not a spec update.
- Never hand-edit a generated document; run its generator.
- Documents stored in `.handoff/` are changed through the handoff tools
  (`handoff_doc_save`, `handoff_doc_update_section`), never by editing the files.
