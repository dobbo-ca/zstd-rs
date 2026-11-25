# Source

Files in `good/` and `bad/` are copied from
`github.com/klauspost/compress`, `compress/zstd/testdata/{good,bad}.zip`,
BSD-3-Clause licensed:

Copyright (c) 2012 The Go Authors. All rights reserved.
Copyright (c) 2019 Klaus Post. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

   * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
   * Redistributions in binary form must reproduce the above
copyright notice, this list of conditions and the following disclaimer
in the documentation and/or other materials provided with the
distribution.
   * Neither the name of Google Inc. nor the names of its
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

# Relabeling

Files kept their original names (dropping klauspost's own good/bad split).
Every file was re-labeled by running it through the zstd 1.5.7 CLI
(`zstd -d -c | cmp`, or `zstd -t`):

- `good/<name>.zst` + `good/<name>.orig`: the CLI decodes the frame and
  `.orig` is that decoded output, byte for byte.
- `bad/<name>.zst`: the CLI rejects the frame (any error).

One klauspost "good" file (`2274d31e0d569fe9e31bedc4f9fddd9c9f114c2f.zst`,
single-segment frame with a truncated compressed block) is filed under
`bad/` here: the zstd 1.5.7 CLI rejects it ("Src size is incorrect"), so
that is the behavior our decoder must match, regardless of klauspost's own
folder name for it.
