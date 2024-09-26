<h1 align="center">Redux</h1>
<p align="center">/ˈriː.dʌks/ <em>(adj.)</em>: brought back</p>

> NOTE: Still in-development!
> 
> * Expect API changes.  (You may need to update your dofiles.)
> * Expect database format changes.  (You may need to delete your redux dir.)
> * There's no CHANGELOG yet.
> 
> Don't use redux for anything serious until it hits version 1.0.

Redux is an implementation of djb's "redo" build tool.  The way you use it is
similar to other redo implementations ([see below][deviations] for deviations);
but internally it works quite differently.  Specifically, when redux builds a
file, it also caches it.  Thereafter, the contents can simply be copied out of
the cache whenever the sources match.  This means that:

* switching to a branch and back again doesn't trigger a rebuild
* reverting a commit doesn't trigger a rebuild
* creating a new worktree doesn't trigger a rebuild

As well as reusing pre-built dependencies, it also supports "early cutoff".
This means that, if you add a comment to a source file, redux can notice
that the resulting object file is unchanged and skip the rest of the build.
(Achieving this with redo requires [extra steps][early cutoff] but redux does
it automatically.)

In the language of [Build systems à la carte], most redo implementations use
"verifying traces", whereas redux uses "constructive traces".  (Like other
redos, it uses a "suspending" scheduler and supports monadic dependencies.)

[deviations]: #differences_from_apenwarrs_redo
[apenwarr]: https://github.com/apenwarr/redo
[Build systems à la carte]: https://www.cambridge.org/core/services/aop-cambridge-core/content/view/097CE52C750E69BD16B78C318754C7A4/S0956796820000088a.pdf/build-systems-a-la-carte-theory-and-practice.pdf
[early cutoff]: https://redo.readthedocs.io/en/latest/FAQSemantics/#if-a-target-is-identical-after-rebuilding-how-do-i-prevent-dependents-from-being-rebuilt

## Differences from apenwarr's redo

redo                   | redux             
-----------------------|-------------------
`redo-ifchange <path>` | `redux <path>`
`redo-always`          | `redux --always`
`redo-stamp`           | `redux --stamp`
`redo-whichdo`         | `redux --whichdo`

In addition:

* stdout is _not_ redirected to the target file.  You need to write to `$3`.
  ([See also][stdout])
* dofiles have to be executable (may change later)

...and, of course, apenwarr's redo has been used and tested by many people for
many years and surely has fewer bugs than redux.

[stdout]: https://redo.readthedocs.io/en/latest/FAQSemantics/#isnt-it-confusing-to-capture-stdout-by-default

### Dofiles are only run for their output

apenwarr's redo supports the case of dofiles which are run for their
side-effects, like "test.do" or "clean.do".  ([See also][unchanged])

[unchanged]: https://redo.readthedocs.io/en/latest/FAQSemantics/#why-does-redo-target-redo-even-unchanged-targets

Redux doesn't support this use-case - if you invoke `redux <target>`, it's
because you want to ensure that `<target>` is up-to-date with its sources.  As
a result:

* there's no `redo`/`redo-ifchange` distinction: redux will _always_ skip running
  the dofile if possible.
* dofiles are always expected to produce some output when run to completion.
  (It's OK for the output file to be empty, but the dofile should create the
  file at least.)
* redux may even start running the dofile and then bail out, part way through!
  (This happens when it becomes clear that an existing cached file can be
  re-used, and that running the remainder of the dofile is unnecessary.)  In
  this case, anything already written to `$3` will be discarded.

If you have scripts which you want to run for their side-effects, there's no
need for redux: just make a normal script ("clean.sh", "test.py", etc.).

### Must be run from a git repo

Redux integrates with the local git repo.  This provides a few benefits:

* It tells us where the "top level" of the project is.  ([See also][top-level])
* It gives us a convenient place to store the database: inside the .git
  directory.
  * This means you don't even need to add anything to your .gitignore
    file.
  * It also gives us the cross-workspace sharing mentioned in the introduction
    "for free".
* It gives us a sensible way to detect whether a file should be generated, or
  simply marked as a source: check whether it's checked-in or not.

I'll expand on that last point a bit.  To mark a source file as a dependency
of the current job, you run `redux <path>`.  To build an intermediate file, you
run `redux <path>`.  There's no difference!  This is a good thing, as it makes
dofiles easier to write.

redux handles these cases differently, however, so we need a way to guess
whether a given file is a source or a generated file. You might say "if there's
a matching dofile, then it should be generated." But this falls down for
projects which use a top-level "default.do".  In such projects, _all_ files
would be marked as generated!

But, with access to a git repo, this issue goes away: we can simply ask git
whether a file is a source or not.

[top-level]: https://redo.readthedocs.io/en/latest/FAQImpl/#how-does-redo-store-dependencies

### Log linearisation

...is not implemented yet.

### Other differences

One final difference in implementation details: redo stores its database [as a
sqlite file][sqlite], which is perfectly sensible; but our database format is
even simpler.  Take a peek in your .git/redux/ and see for yourself!

[sqlite]: https://redo.readthedocs.io/en/latest/FAQImpl/#isnt-using-sqlite3-overkill-and-un-djb-ish

## How it works

### How it works: Building a file

You ask redux to build `foo/bar.txt`.

1. It searches for the rule for that file.  Let's say it's `default.txt.do`.
2. It creates a temporary file `foo/.redux_bar.txt.tmp` for the script to write to
3. It runs the rule like so: `default.txt.do bar foo/bar.txt foo/.redux_bar.txt.tmp`
4. `default.txt.do` writes into the temp file and returns exit code 0
5. redux renames the temp file to `foo/bar.txt`

If the script returned a non-zero exit code, the temp file would instead be
deleted.

### How it works: Rule selection

Same as redo.  You can inspect this with `redux --whichdo`.

### How it works: Recording a trace

When redux runs a rule, it creates two files: a temp file for the script to
write to, as explained above; and a "tracefile".  So in our example, it would
create the following files:

* `foo/.redux_bar.txt.tmp`
* `foo/.redux_bar.txt.trace`

As the script runs, it records a "trace", which is a log of the files it read
as it ran.  Specifically, whenever you run `redux`, it writes a line into the
tracefile recording the hash of the given file.

When the script finishes, redux renames the temp file, as explained above.  It
also adds one final line to the tracefile, recording the hash of the produced
file.  It then moves the tracefile to the "trace store" (.git/redux/traces/).

### How it works: Logging

TODO: implement (systemd-run and the journal)

### How it works: Skipping if up-to-date

TODO: document

### How it works: Early-termination if output is known

TODO: document

### How it works: Volatile builds

TODO: document

## Building from a git commit

TODO: implement

## Sharing the build cache

The build cache is linked to the git repository, which means it is automatically
shared between all worktrees.  In theory the cache could be shared between
multiple machines or even multiple people by storing it in S3 (a la nix's
"substituters" system), but this isn't implemented.
