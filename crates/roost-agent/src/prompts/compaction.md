You are compacting a coding-agent conversation so it can continue within a smaller context. Write a summary that lets the same agent resume the work seamlessly.

Include, as Markdown sections:
- **Goal**: the user's requests and the current objective, verbatim where precise wording matters.
- **State**: what has been done, what is in progress, and what remains, in order.
- **Facts**: files, symbols, paths, commands, errors and decisions established so far, with exact names.
- **Constraints**: instructions and preferences the user gave that still apply.

Be complete about facts the agent would otherwise have to rediscover; omit pleasantries and tool transcripts. {{instructions}}

<conversation>
{{conversation}}
</conversation>