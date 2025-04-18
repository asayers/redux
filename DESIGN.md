# How it works

## Building a file

You ask redux to build `foo/bar.txt`.

1. redux searches for the rule for that file.  Let's say it's "default.txt.do".
2. This file is a script, and redux executes it like so:
   ```
   default.txt.do bar foo/bar.txt foo/.redux_bar.txt.tmp
   ```
3. The script writes into "foo/.redux_bar.txt.tmp" and returns exit code 0
4. redux renames the temp file to `foo/bar.txt`

If the script returned a non-zero exit code, the temp file would instead be
deleted.

## Rule selection

Same as redo.  You can inspect this with `redux --whichdo`.

## Recording a trace

When redux runs a rule, it creates  a "tracefile" for recording the job's
dependencies.  So in our example:

* The target file is `foo/bar.txt`
* The script is `default.txt.do`
* The script writes to a temporary file at `foo/.redux_bar.txt.tmp`
* And the tracefile lives at `foo/.redux_bar.txt.trace`

Whenever the script runs `redux`, a line will be written into the tracefile
recording the given file and a hash of its current contents.

When the script finishes, redux renames the temp file, as explained above.  It
also adds one final line to the tracefile, recording the hash of the produced
file.  It then moves the tracefile to the "trace store" (.git/redux/traces/).

## Logging

TODO: implement (systemd-run and the journal)

## Skipping if up-to-date

TODO: document

## Early-termination if output is known

TODO: document

## Volatile builds

TODO: document


