Send a short message to another conversation without stopping your own work.

`to` is the recipient: `main` (the main conversation), a subagent's name, or a conversation id. `"all"` broadcasts to every peer you can see: your parent and sibling subagents, or your child subagents if you are the main conversation.

Delivery never blocks. A recipient that is running reads the message at its next step boundary. The main conversation starts a run for a direct message when it is idle. A subagent with no run in progress reads a direct message only when its task starts. A broadcast never wakes an idle peer; it waits in the journal. A subagent whose run has ended cannot receive messages.

Use this for quick coordination, not long reports. Subagents: your final result still goes to your parent through `yield`.
