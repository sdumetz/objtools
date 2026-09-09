# Goals

This workspace should provide a binary that will read a very large (More than a few times the RAM+ swap space of this machine) wavefront OBJ file to extract its metadata.

it HAS to process the file in a streaming manner (line-by-line or chunk-by-chunk) or it will trigger OOM errors.

It can be written in any language, but good support for streaming file reading/writing is the main factor to take into account. Good memory management could help.

First thing I want to extract from my very large OBJ is the number of different objects it has (`o` lines in OBJ syntax), with the number of vertices for each object and the list of materials each object has.


