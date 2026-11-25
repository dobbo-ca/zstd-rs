// Package hook lets package czi observe block progress in a zstd.Decoder
// without widening the public API.
package hook

// SetBlockDone is set by package zstd. fn, if not nil, is called after each
// block with the output decoded so far.
var SetBlockDone func(d any, fn func(out []byte))
