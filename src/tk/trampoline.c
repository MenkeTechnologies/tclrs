/*
 * The variadic stub slots, marshalled into a shape Rust can be handed.
 *
 * Seven slots of `TclStubs` are C-variadic. Stable rustc refuses to *define*
 * such a function outright:
 *
 *     error[E0658]: C-variadic functions are unstable
 *       = note: see issue #44930 <https://github.com/rust-lang/rust/issues/44930>
 *
 * and on AAPCS64 there is no way to cheat around that from Rust either, because
 * a variadic argument is passed on the stack while a fixed one of the same
 * position would have been in a register (Procedure Call Standard for the Arm
 * 64-bit Architecture, §6.4.2: "the variadic arguments are laid out on the
 * stack"), so no non-variadic declaration can name one. A C file compiled by
 * `build.rs` is the fix: it is the only place in the tree that may write
 * `va_arg`, and everything it reads is handed on as a counted array or a
 * finished string.
 *
 * Only the slots whose variadic arguments carry a payload are here:
 *
 *   Tcl_AppendStringsToObj  (slot 15) — Tk builds a fully qualified command
 *       name out of them (tk9.0.4/generic/tkUtil.c:1222), so a body that
 *       ignored them would register every ensemble subcommand under the
 *       ensemble's own name.
 *   Tcl_Panic               (slot 2)  — the message is the whole content of
 *       the call. Tk calls it 227 times and it never returns, so an abort
 *       without the formatted text is an abort with no diagnosis.
 *
 * The remaining five are argued about, not marshalled; see `tk::eval`.
 */

#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>

/* Implemented in Rust: `src/tk/eval.rs`. */
extern void tclrs_tk_append_strings(void *obj_ptr, const char *const *strings,
				    size_t count);
extern void tclrs_tk_panic(const char *text);

/*
 * Tcl's own loop over the argument list ends at the first NULL and has no upper
 * bound (generic/tclStringObj.c:1820-1828). A fixed ceiling here is a refusal
 * to walk off the end of a list that was not terminated: the longest call in Tk
 * passes two strings, so nothing legitimate comes near it.
 */
#define TCLRS_TK_MAX_STRINGS 64

/* Length of the buffer a panic message is formatted into. Tcl's own
 * `Tcl_Panic` writes through vfprintf with no limit; a truncated diagnostic is
 * still a diagnostic, and this process is about to abort either way. */
#define TCLRS_TK_PANIC_BUF 4096

/*
 * void Tcl_AppendStringsToObj(Tcl_Obj *objPtr, ...) — generic/tclDecls.h:92.
 *
 * A NULL-terminated list of `char *`, appended in order
 * (generic/tclStringObj.c:1808-1829).
 */
void
tclrs_tk_append_strings_to_obj(void *obj_ptr, ...)
{
	const char *args[TCLRS_TK_MAX_STRINGS];
	size_t count = 0;
	va_list ap;

	va_start(ap, obj_ptr);
	while (count < TCLRS_TK_MAX_STRINGS) {
		const char *s = va_arg(ap, const char *);

		if (s == NULL) {
			break;
		}
		args[count++] = s;
	}
	va_end(ap);

	tclrs_tk_append_strings(obj_ptr, args, count);
}

/*
 * TCL_NORETURN void Tcl_Panic(const char *format, ...) —
 * generic/tclDecls.h:62. A printf format and its arguments; Tcl's own body
 * formats them and then aborts (generic/tclPanic.c).
 */
void
tclrs_tk_panic_trampoline(const char *format, ...)
{
	char buf[TCLRS_TK_PANIC_BUF];
	va_list ap;

	va_start(ap, format);
	vsnprintf(buf, sizeof buf, format, ap);
	va_end(ap);

	tclrs_tk_panic(buf);
}
