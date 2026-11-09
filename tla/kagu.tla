-------------------------------- MODULE kagu --------------------------------
(* Run lifecycle, workspace locks and release, host crash and recovery, and
   log subscription, as specified in docs/spec.md.

   Abstracted: checks (checking ends in completed), permission policy (one
   permission round per run, which keeps the log finite), purge, payloads. *)
EXTENDS Naturals, Sequences

CONSTANTS
    Runs,       \* run ids
    Overlap,    \* pairs of runs whose workspaces overlap; symmetric
    Contained,  \* runs whose substrate contains the process tree
    MaxCrashes  \* host crashes in a behaviour; liveness assumes the host
                \* eventually stays up

NonTerminal == {"submitted", "working", "input_required", "checking"}
Terminal    == {"completed", "failed", "canceled", "rejected", "interrupted"}
States      == {"none"} \cup NonTerminal \cup Terminal
Admitted    == {"working", "input_required", "checking"}

(* A run's process tree as it really is; the host may not know.
   none     never started
   alive    root running
   orphans  root gone, descendants may still run (uncontained only)
   gone     whole tree gone *)
Procs == {"none", "alive", "orphans", "gone"}

VARIABLES
    st,          \* run state held by the run actors
    proc,        \* process trees
    released,    \* run.released recorded
    attested,    \* released by workspace.release rather than observed
    up,          \* host process running
    recovering,  \* host started, recovery not finished
    log,         \* durable, insert-only event log
    sub,         \* client subscribed
    cursor,      \* last seq the client processed
    view,        \* run states as the client has folded them
    crashes      \* host crashes so far

vars == <<st, proc, released, attested, up, recovering, log, sub, cursor, view, crashes>>
host == <<st, proc, released, attested, up, recovering, log, crashes>>
client == <<sub, cursor, view>>

Ev(r, t, to) == [run |-> r, type |-> t, to |-> to]

Fold(l) == [r \in Runs |->
    LET idx == {i \in 1..Len(l) : l[i].run = r /\ l[i].type = "state"}
    IN IF idx = {} THEN "none" ELSE l[CHOOSE i \in idx : \A j \in idx : j <= i].to]

ReleasedIn(l) == [r \in Runs |->
    \E i \in 1..Len(l) : l[i].run = r /\ l[i].type = "released"]

Asked(r) == \E i \in 1..Len(log) : log[i].run = r /\ log[i].to = "input_required"

\* Invariant 8: held from admission until release.
Holds(r) == \/ st[r] \in Admitted
            \/ st[r] \in Terminal /\ proc[r] /= "none" /\ ~released[r]

Live(r) == proc[r] \in {"alive", "orphans"}

Conflict(r) == \E q \in Runs : q /= r /\ <<r, q>> \in Overlap /\ Holds(q)

Ready == up /\ ~recovering

\* A run actor changes its run's state: event first (invariant 1).
Move(r, to) ==
    /\ st' = [st EXCEPT ![r] = to]
    /\ log' = Append(log, Ev(r, "state", to))

TypeOK ==
    /\ st \in [Runs -> States]
    /\ proc \in [Runs -> Procs]
    /\ released \in [Runs -> BOOLEAN]
    /\ attested \in [Runs -> BOOLEAN]
    /\ up \in BOOLEAN
    /\ recovering \in BOOLEAN
    /\ sub \in BOOLEAN
    /\ cursor \in 0..Len(log)
    /\ view \in [Runs -> States]
    /\ crashes \in 0..MaxCrashes

Init ==
    /\ st = [r \in Runs |-> "none"]
    /\ proc = [r \in Runs |-> "none"]
    /\ released = [r \in Runs |-> FALSE]
    /\ attested = [r \in Runs |-> FALSE]
    /\ up = TRUE
    /\ recovering = FALSE
    /\ log = <<>>
    /\ sub = FALSE
    /\ cursor = 0
    /\ view = [r \in Runs |-> "none"]
    /\ crashes = 0

----------------------------------------------------------------------------
(* Host *)

Submit(r) ==
    /\ Ready /\ st[r] = "none"
    /\ Move(r, "submitted")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

\* Admission locks the workspace and starts the worker.
Admit(r) ==
    /\ Ready /\ st[r] = "submitted"
    /\ Move(r, "working")
    /\ proc' = [proc EXCEPT ![r] = "alive"]
    /\ UNCHANGED <<released, attested, up, crashes, recovering>> /\ UNCHANGED client

\* Workspace held, or a required level unmet.
Reject(r) ==
    /\ Ready /\ st[r] = "submitted"
    /\ Move(r, "rejected")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

Ask(r) ==
    /\ Ready /\ st[r] = "working" /\ proc[r] = "alive" /\ ~Asked(r)
    /\ Move(r, "input_required")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

Decide(r) ==
    /\ Ready /\ st[r] = "input_required" /\ proc[r] = "alive"
    /\ Move(r, "working")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

\* session/prompt returns end_turn.
EndTurn(r) ==
    /\ Ready /\ st[r] = "working" /\ proc[r] = "alive"
    /\ Move(r, "checking")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

\* Any other stop reason, or a JSON-RPC error.
StopOther(r) ==
    /\ Ready /\ st[r] = "working" /\ proc[r] = "alive"
    /\ Move(r, "failed")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

Verdict(r) ==
    /\ Ready /\ st[r] = "checking"
    /\ Move(r, "completed")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

Cancel(r) ==
    /\ Ready /\ st[r] \in NonTerminal
    /\ Move(r, "canceled")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

\* The host kills the tree of a run that has ended, and during recovery any
\* recorded root still alive. Uncontained, only the root is known to die.
Kill(r) ==
    /\ up /\ proc[r] = "alive"
    /\ st[r] \in Terminal \/ recovering
    /\ proc' = [proc EXCEPT ![r] = IF r \in Contained THEN "gone" ELSE "orphans"]
    /\ UNCHANGED <<st, released, attested, up, crashes, recovering, log>> /\ UNCHANGED client

\* run.released once the tree is confirmed gone: contained and the root has
\* exited, or never started.
Release(r) ==
    /\ Ready /\ st[r] \in Terminal /\ ~released[r]
    /\ \/ proc[r] = "none"
       \/ r \in Contained /\ proc[r] = "gone"
    /\ released' = [released EXCEPT ![r] = TRUE]
    /\ log' = Append(log, Ev(r, "released", "none"))
    /\ UNCHANGED <<st, proc, attested, up, crashes, recovering>> /\ UNCHANGED client

\* workspace.release: a client attests an uncontained tree is gone.
Attest(r) ==
    /\ Ready /\ st[r] \in Terminal /\ ~released[r] /\ proc[r] /= "none"
    /\ r \notin Contained
    /\ released' = [released EXCEPT ![r] = TRUE]
    /\ attested' = [attested EXCEPT ![r] = TRUE]
    /\ log' = Append(log, Ev(r, "released", "none"))
    /\ UNCHANGED <<st, proc, up, crashes, recovering>> /\ UNCHANGED client

----------------------------------------------------------------------------
(* Worker and environment: these happen whether or not the host is up; the
   host records them only when it observes them. *)

\* The root exits. Contained: the namespace and every descendant end with it.
RootExits(r) ==
    /\ proc[r] = "alive"
    /\ proc' = [proc EXCEPT ![r] = IF r \in Contained THEN "gone" ELSE "orphans"]
    /\ IF Ready /\ st[r] \in {"working", "input_required"}
          THEN Move(r, "failed")
          ELSE UNCHANGED <<st, log>>
    /\ UNCHANGED <<released, attested, up, crashes, recovering>> /\ UNCHANGED client

\* Orphaned descendants end. Nobody observes it.
OrphansEnd(r) ==
    /\ proc[r] = "orphans"
    /\ proc' = [proc EXCEPT ![r] = "gone"]
    /\ UNCHANGED <<st, released, attested, up, crashes, recovering, log>> /\ UNCHANGED client

\* The host dies. Contained trees die with it (bwrap --die-with-parent ends
\* the PID namespace). An uncontained root may survive or leave orphans.
Crash ==
    /\ up /\ crashes < MaxCrashes
    /\ crashes' = crashes + 1
    /\ up' = FALSE
    /\ recovering' = FALSE
    /\ \E survive \in SUBSET {r \in Runs : r \notin Contained /\ proc[r] = "alive"} :
          proc' = [r \in Runs |->
                     IF proc[r] /= "alive" \/ r \in survive THEN proc[r]
                     ELSE IF r \in Contained THEN "gone" ELSE "orphans"]
    /\ sub' = FALSE
    /\ UNCHANGED <<st, released, attested, log, cursor, view>>

Restart ==
    /\ ~up
    /\ up' = TRUE
    /\ recovering' = TRUE
    /\ UNCHANGED <<st, proc, released, attested, log, crashes>> /\ UNCHANGED client

\* Recovery: each non-terminal run's actor records run.interrupted. Nothing
\* about the worker's end is recorded (invariant 9).
Interrupt(r) ==
    /\ up /\ recovering /\ st[r] \in NonTerminal
    /\ Move(r, "interrupted")
    /\ UNCHANGED <<proc, released, attested, up, crashes, recovering>> /\ UNCHANGED client

Recovered ==
    /\ up /\ recovering /\ \A r \in Runs : st[r] \notin NonTerminal
    /\ recovering' = FALSE
    /\ UNCHANGED <<st, proc, released, attested, up, crashes, log>> /\ UNCHANGED client

----------------------------------------------------------------------------
(* Client. log.subscribe reads a snapshot and its seq in one read
   transaction; with `after`, the client resumes from its own cursor. *)

SubscribeFresh ==
    /\ Ready /\ ~sub
    /\ sub' = TRUE
    /\ view' = Fold(log)
    /\ cursor' = Len(log)
    /\ UNCHANGED host

SubscribeAfter ==
    /\ Ready /\ ~sub
    /\ sub' = TRUE
    /\ UNCHANGED <<cursor, view>> /\ UNCHANGED host

Deliver ==
    /\ up /\ sub /\ cursor < Len(log)
    /\ cursor' = cursor + 1
    /\ LET e == log[cursor + 1]
       IN view' = IF e.type = "state" THEN [view EXCEPT ![e.run] = e.to] ELSE view
    /\ UNCHANGED sub /\ UNCHANGED host

Disconnect ==
    /\ sub
    /\ sub' = FALSE
    /\ UNCHANGED <<cursor, view>> /\ UNCHANGED host

----------------------------------------------------------------------------

Next ==
    \/ \E r \in Runs :
          \/ Submit(r) \/ Admit(r) \/ Reject(r) \/ Ask(r) \/ Decide(r)
          \/ EndTurn(r) \/ StopOther(r) \/ Verdict(r) \/ Cancel(r)
          \/ Kill(r) \/ Release(r) \/ Attest(r) \/ Interrupt(r)
          \/ RootExits(r) \/ OrphansEnd(r)
    \/ Crash \/ Restart \/ Recovered
    \/ SubscribeFresh \/ SubscribeAfter \/ Deliver \/ Disconnect

Fairness ==
    /\ WF_vars(Restart)
    /\ WF_vars(Recovered)
    /\ \A r \in Runs :
          /\ WF_vars(Interrupt(r))
          /\ SF_vars(Kill(r))
          /\ SF_vars(Release(r))

Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
(* Properties *)

\* Invariant 1: state is a fold over the log.
LogFirst == st = Fold(log) /\ released = ReleasedIn(log)

\* Invariant 8.
LockExclusive ==
    \A r, q \in Runs : r /= q /\ <<r, q>> \in Overlap => ~(Holds(r) /\ Holds(q))

\* What the lock is for: two process trees never share a workspace, unless
\* a client attested a release that was false.
WriterExclusive ==
    \A r, q \in Runs : r /= q /\ <<r, q>> \in Overlap =>
        ~(Live(r) /\ Live(q)) \/ attested[r] \/ attested[q]

\* A client that has caught up sees exactly the host's states.
ClientConsistent == sub /\ cursor = Len(log) => view = st

\* Invariant 4.
TerminalFinal == [][\A r \in Runs : st[r] \in Terminal => st'[r] = st[r]]_vars

\* Invariant 9: a run reaches interrupted only during recovery.
InterruptedOnRecovery ==
    [][\A r \in Runs : st'[r] = "interrupted" /\ st[r] /= "interrupted" => recovering]_vars

\* A contained run that ends is eventually released.
ContainedReleased ==
    \A r \in Contained : st[r] \in Terminal \ {"rejected"} ~> released[r]

=============================================================================
