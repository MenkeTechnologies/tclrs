#!/usr/bin/env perl
# Vendor the character-class and collating-name tables of Tcl's regular
# expression compiler into src/regc_locale.rs.
#
# `[[:alpha:]]`, `\d`, `\s`, `\w` and `[[.name.]]` in a Tcl regular expression
# are defined by the tables in generic/regc_locale.c — not by the Unicode
# tables `string is` uses (generic/tclUniData.c), and not by the `regex`
# crate's. The tables are the specification, so none of them is typed by hand:
# every range below is lifted verbatim from the Tcl source release that
# conformance/fetch-suite.sh fetches and verifies against a pinned SHA-256.
# Tcl 9 builds with CHRBITS 32, so the `#if CHRBITS > 16` tails are included.
#
# Usage, from the crate root:
#
#     sh conformance/fetch-suite.sh
#     perl scripts/gen_regc_locale.pl
#
# The output is committed. Re-run it only to move to a different Tcl release.

use strict;
use warnings;

my $version = $ENV{TCL_VERSION} // '9.0.4';
my $src = "conformance/vendor/tcl$version/generic/regc_locale.c";
my $out = 'src/regc_locale.rs';

open my $in, '<', $src or die "$src: $!\n";
my $c = do { local $/; <$in> };
close $in;

my @emit;

# The collating-element names: `{"name", 'c'},`.
my ($cnames) = $c =~ /cnames\[\]\s*=\s*\{(.*?)\{NULL/s or die "no cnames table\n";
my @names;
while ($cnames =~ /\{"([^"]+)",\s*'((?:\\x[0-9A-Fa-f]{2}|\\.|.))'\}/g) {
    my ($name, $lit) = ($1, $2);
    my $code = $lit =~ /^\\x([0-9A-Fa-f]{2})$/ ? hex $1
             : $lit =~ /^\\(.)$/              ? ord $1
             :                                  ord $lit;
    push @names, sprintf('    ("%s", 0x%02X),', $name, $code);
}
die "cnames: nothing parsed\n" unless @names;
push @emit, "/// `cnames[]`: the multi-character collating-element names `[[.name.]]` accepts.";
push @emit, 'pub(crate) const CNAMES: &[(&str, u32)] = &[', @names, '];', '';

for my $class (qw(alpha control digit punct space lower upper graph)) {
    my $uc = uc $class;
    if ($c =~ /static const crange ${class}RangeTable\[\]\s*=\s*\{(.*?)\};/s) {
        my $body = $1;
        $body =~ s/#if CHRBITS > 16//g;
        $body =~ s/#endif//g;
        my @r;
        push @r, "($1, $2)" while $body =~ /\{(0x[0-9A-Fa-f]+),\s*(0x[0-9A-Fa-f]+)\}/g;
        die "$class ranges: nothing parsed\n" unless @r;
        push @emit, "/// `${class}RangeTable`.";
        push @emit, "pub(crate) const ${uc}_RANGES: &[(u32, u32)] = &[";
        push @emit, map { "    $_," } @r;
        push @emit, '];', '';
    }
    if ($c =~ /static const chr ${class}CharTable\[\]\s*=\s*\{(.*?)\};/s) {
        my $body = $1;
        $body =~ s/#if CHRBITS > 16//g;
        $body =~ s/#endif//g;
        my @ch = $body =~ /(0x[0-9A-Fa-f]+)/g;
        die "$class chars: nothing parsed\n" unless @ch;
        push @emit, "/// `${class}CharTable`.";
        push @emit, "pub(crate) const ${uc}_CHARS: &[u32] = &[";
        push @emit, map { "    $_," } @ch;
        push @emit, '];', '';
    }
}

open my $o, '>', $out or die "$out: $!\n";
print $o "//! Character-class and collating-name tables of Tcl's regular expression\n";
print $o "//! compiler, lifted verbatim from `generic/regc_locale.c` of Tcl $version by\n";
print $o "//! `scripts/gen_regc_locale.pl`. Do not edit by hand: regenerate.\n\n";
print $o "#![allow(clippy::unreadable_literal)]\n\n";
print $o join("\n", @emit);
close $o;
print "wrote $out\n";
