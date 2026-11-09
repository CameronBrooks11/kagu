---- MODULE MC ----
(* Two runs on overlapping workspaces, one contained and one not; at most
   two host crashes. *)
EXTENDS kagu

MCRuns == {"r1", "r2"}
MCOverlap == {<<"r1", "r2">>, <<"r2", "r1">>}
MCContained == {"r1"}
====
