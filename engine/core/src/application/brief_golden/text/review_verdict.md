## Verdict

Post the verdict as one `work_comment` on this item. Its first line is exactly one of:

- `no blockers (head <sha>)` — every line of **Done means** holds at that head;
- `blockers (head <sha>):` followed by a numbered list, one blocker a line, each naming the line of **Done means** it fails and the place in the diff.

`<sha>` is the commit you reviewed. Then say **which deployment paths the tests cover**: in-process only, or the deployed topology described under **Deployed setup**. A test that passed in-process is not evidence for a path it never ran. Do not write `no blockers` while describing a regression or a gap — that is a blocker.
