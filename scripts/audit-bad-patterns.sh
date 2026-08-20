#!/usr/bin/env bash
# audit-bad-patterns.sh — run the docs/BAD_PATTERNS.md regex matrix against
# every .rs file under src/.
#
# Usage:
#   scripts/audit-bad-patterns.sh                     # full report to stdout
#   scripts/audit-bad-patterns.sh --strict            # exit non-zero on any gated hit
#   scripts/audit-bad-patterns.sh --section A         # run only one section
#   scripts/audit-bad-patterns.sh --section A,D,P     # run several sections
#   scripts/audit-bad-patterns.sh --files <listfile>  # audit paths named in <listfile>
#   scripts/audit-bad-patterns.sh --strict-sections ALL
#   scripts/audit-bad-patterns.sh --show              # print file:line for every hit
#
# Sections (from docs/BAD_PATTERNS.md):
#   A: Panicking error handling      I: Iterator glitches
#   B: Silent error swallowing       J: Style / secrets
#   C: Lint suppression              L: Crypto
#   D: Panic vectors (index/slice)   M: SQL & command injection
#   E: Numeric / unsafe              N: UB / concurrency
#   F: Async / blocking              O: Performance
#   G: Logging                       P: API hygiene
#   H: HTTP layer
#
# Exit codes: 0 ok | 1 strict failure | 2 usage error | 3 nothing audited / internal error
#
# Waivers: append `// audit-allow: <reason>` to a line to exclude it.

set -uo pipefail

die() { printf 'audit: %s\n' "$1" >&2; exit "${2:-3}"; }
usage() { sed -n '2,/^# Waivers:/p' "$0" | sed 's/^# \{0,1\}//'; }

# ---- locate repo root ----------------------------------------------------
_here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd) \
    || die "cannot resolve repo root from ${BASH_SOURCE[0]}"
[ -n "$_here" ] || die "repo root resolved to an empty path"
ROOT=$_here
cd -- "$ROOT" || die "cannot cd to $ROOT"

command -v perl >/dev/null 2>&1 || die "perl is required (regex engine + Rust comment stripper)"

# ---- argument parsing ----------------------------------------------------
STRICT=0
ONLY_SECTIONS=""
FILE_LIST=""
STRICT_OVERRIDE=""
SHOW=0

need_arg() { [ "$2" -ge 2 ] || { printf 'audit: %s requires a value\n' "$1" >&2; exit 2; }; }

while [ $# -gt 0 ]; do
    case "$1" in
        --strict)           STRICT=1; shift ;;
        --show)             SHOW=1; shift ;;
        --section)          need_arg "$1" $#; ONLY_SECTIONS="$2"; shift 2 ;;
        --section=*)        ONLY_SECTIONS="${1#*=}"; shift ;;
        --files)            need_arg "$1" $#; FILE_LIST="$2"; shift 2 ;;
        --files=*)          FILE_LIST="${1#*=}"; shift ;;
        --strict-sections)  need_arg "$1" $#; STRICT_OVERRIDE="$2"; shift 2 ;;
        --strict-sections=*) STRICT_OVERRIDE="${1#*=}"; shift ;;
        -h|--help)          usage; exit 0 ;;
        --)                 shift; break ;;
        *)                  printf 'audit: unknown arg: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done
[ $# -eq 0 ] || { printf 'audit: unexpected operand: %s\n' "$1" >&2; exit 2; }

# ---- file discovery (NUL-safe) -------------------------------------------
TMPDIR_AUDIT=$(mktemp -d "${TMPDIR:-/tmp}/audit-bad-patterns.XXXXXX") || die "mktemp failed"
trap 'rm -rf -- "$TMPDIR_AUDIT"' EXIT INT TERM
FILELIST_NUL="$TMPDIR_AUDIT/files.nul"
PATSPEC="$TMPDIR_AUDIT/patterns.tsv"
PERL_SRC="$TMPDIR_AUDIT/audit.pl"

MISSING=0
if [ -n "$FILE_LIST" ]; then
    [ -f "$FILE_LIST" ] || die "file list '$FILE_LIST' does not exist" 2
    : > "$FILELIST_NUL"
    n=0
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in ''|'#'*) continue ;; esac
        if [ -f "$line" ]; then
            printf '%s\0' "$line" >> "$FILELIST_NUL"
            n=$((n + 1))
        else
            printf 'audit: warning: listed path not found, skipping: %s\n' "$line" >&2
            MISSING=$((MISSING + 1))
        fi
    done < "$FILE_LIST"
    [ "$n" -gt 0 ] || die "file list '$FILE_LIST' yielded no readable files"
else
    [ -d src ] || die "no src/ directory under $ROOT — refusing to report a clean tree"
    find src -name '*.rs' -type f -print0 > "$FILELIST_NUL" \
        || die "find failed while enumerating src/*.rs"
fi

N_FILES=$(tr -dc '\0' < "$FILELIST_NUL" | wc -c | tr -d ' ')
[ "$N_FILES" -gt 0 ] || die "0 Rust files matched — refusing to report a clean tree"

# ---- pattern arrays ------------------------------------------------------
# Patterns are PCRE (matched by perl). `L<TAB>` = per-line, `M<TAB>` = whole-file
# multi-line. Comments and `// audit-allow:` lines are removed before matching;
# string literals are preserved.

A=(  # Panicking error handling — explicit () anchors so we don't match the
     # value-providing _or family.
    '\.unwrap\(\)'  '\.expect\('  '\.unwrap_or_default\(\)'
    '\.parse\(\)\.unwrap\(\)'  '\.parse::<[^>]+>\(\)\.unwrap\(\)'
    '\.try_into\(\)\.unwrap\(\)'
    '\.first\(\)\.unwrap\(\)'  '\.last\(\)\.unwrap\(\)'
    '\.next\(\)\.unwrap\(\)'
    '\.iter\(\)\.[a-z_]+\([^)]*\)\.unwrap\(\)'
    '\.chars\(\)\.next\(\)\.unwrap\(\)'
    '\.split\([^)]*\)\.next\(\)\.unwrap\(\)'
    '\.position\([^)]*\)\.unwrap\(\)'
    '\.iter\(\)\.find\([^)]*\)\.unwrap\(\)'
    '\.read_to_string\([^)]*\)\.unwrap\(\)'
    '\.lock\(\)\.unwrap\(\)'
    '\.expect_err\('  '\.unwrap_err\('
    'panic!\('  'unreachable!\('  'todo!\('  'unimplemented!\('
    '(?<![\w!])assert!\('  '(?<![\w!])assert_eq!\('  '(?<![\w!])assert_ne!\('
    '(?<![\w!])debug_assert(?:_eq|_ne)?!\('
)
B=(  # Silent error swallowing
    'Err\(_\)'  'Err\(_[a-zA-Z]\w*\)'  'if let Err\(_'
    'if let Ok\('  'let\s+_\s*='  'let\s+_[a-zA-Z]\w*\s*=.*\.await'
    '\.map_err\(\|_\|'  '\.or_else\(\|_\|'
    '\.to_str\(\)\.ok\(\)'  '\.json\([^)]*\)\.await\.ok\(\)'
    '\.send\(\)\.await\.ok\(\)'  '\.text\(\)\.await\.ok\(\)'
)
C=(  # Lint suppression
    '#\[allow\('  '#\[deny\('  '#\[ignore\b'
)
D=(  # Panic vectors (index/slice). NOTE: indexing on the left-hand side of an
     # assignment panics too, so it is deliberately NOT excluded here.
    '\b[a-zA-Z_]\w*\[\s*\d+\s*\]'
    '&[a-zA-Z_]\w*\[\.\.\w+\]'
    '&[a-zA-Z_]\w*\[\w+\.\.\]'
    '&[a-zA-Z_]\w*\[\w+\.\.\w+\]'
    '\.split_at\('  '\.chars\(\)\.nth\('
)
E=(  # Numeric / unsafe
    '\bas\s+u8\b'  '\bas\s+i8\b'
    '\bas\s+u16\b'  '\bas\s+i16\b'
    '\bas\s+u32\b'  '\bas\s+i32\b'
    '\bas\s+u64\b'  '\bas\s+i64\b'
    '\bas\s+usize\b'  '\bas\s+isize\b'
    '\bas\s+f32\b'  '\bas\s+f64\b'
    '\bas\s+\*const\b'  '\bas\s+\*mut\b'
    '\btransmute\('  '\bextern\s+"C"'
    '\bunsafe\s*\{'  '\bunsafe\s+fn\b'
)
F=(  # Async / blocking
    'std::thread::sleep'  'std::process::Command'
    'std::fs::File'  'std::fs::read\b'  'std::fs::write\b'
    'std::net::TcpStream'  'std::net::UdpSocket'  'std::io::stdin'
)
G=(  # Logging
    '\bdbg!\('
    'format!\(.*"\{:\?\}".*\b[eE]rr\b'
    '\.context\(""\)'  '\.context\("\?"\)'
)
H=(  # HTTP layer
    'reqwest::Client::new\(\)'  'reqwest::Client::builder\(\)'
    '\.send\(\)\.await\?(?![.?])'  '\.text\(\)\.await\?(?![.?])'
    'format!\("\{:\?\}", \w+\)\.contains\('
)
I=(  # Iterator glitches
    '\.collect::<Result<Vec<_>,\s*_>>\(\)\.unwrap'
    '\.zip\([^)]*\)\.unwrap'
)
J=(  # Style / secrets
    '== ""'  '\.len\(\) == 0'  '\.len\(\) > 0'
    'String::from\(format!'
    '\.clone\(\)\s*\.clone'  '\.to_string\(\)\s*\.to_string'
    '\b(?:XXXXXX|TODO|FIXME|HACK)\b'
    'Bearer\s+[A-Za-z0-9_.-]{40,}'
    'sk-[A-Za-z0-9]{20,}'  'AKIA[A-Z0-9]{16}'
    '"admin"\s*,\s*"admin"'  '"root"\s*,\s*"root"'
    'Box<dyn'
)
L=(  # Crypto
    '\bmd5::compute\b|\bmd5::Md5\b'
    '\bsha1::Sha1\b|use sha1::'
    '\bdes::Des\b|\b3des\b|TripleDES'
    'rand::thread_rng\(\)'  'rand::random\(\)'
    '\bRC4\b|rc4::'  'aes_128_ecb|aes-128-ecb|Ecb'
)
M=(  # Injection
    'format!\("SELECT[^"]*\{'  'format!\("INSERT[^"]*\{'
    'format!\("UPDATE[^"]*\{'  'format!\("DELETE[^"]*\{'
    'std::process::Command::new\("/bin/sh"\)'
    'std::process::Command::new\("sh"\)'
    '\.arg\("-c"\).*format!'
)
N=(  # UB / concurrency
    '\bstatic\s+mut\b'  'std::mem::transmute'  'std::mem::forget'
    'std::mem::uninitialized'  'std::mem::zeroed'
    'std::ptr::read'  'std::ptr::write'
    'Result<[^,<>]*,\s*String>'
)
O=(  # Performance
    '\.iter\(\)\.count\(\)'  '\.collect::<\(\)>\(\)'
    '\.iter\(\)\.map\(\|\w+\|\s*\w+\.clone\(\)\)\s*\.collect'
    '\.to_string\(\)\.as_str\(\)'
    'Vec::with_capacity\(0\)'  'String::with_capacity\(0\)'
    'Regex::new\(.*\)\.unwrap'
    'Box::new\(.*Box::new'
)
P=(  # API hygiene
    '\bpub\s+const\s+\w+\s*:\s*&str\s*=\s*"http'
)
# Whole-file (multi-line) patterns: a real struct definition never fits on one
# line, so these MUST NOT be matched line-by-line.
P_ML=(
    '#\[derive\([^)]*\bDebug\b[^)]*\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?struct\s+\w+\s*\{[^}]*\b[Pp]assword'
    '#\[derive\([^)]*\bDebug\b[^)]*\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?struct\s+\w+\s*\{[^}]*\b[Ss]ecret'
    '#\[derive\([^)]*\bDebug\b[^)]*\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?struct\s+\w+\s*\{[^}]*\b[Tt]oken'
)

declare -A SECTION_DESC=(
    [A]="Panicking error handling"  [B]="Silent error swallowing"
    [C]="Lint suppression"          [D]="Panic vectors (index/slice)"
    [E]="Numeric / unsafe"          [F]="Async / blocking"
    [G]="Logging"                   [H]="HTTP layer"
    [I]="Iterator glitches"         [J]="Style / secrets"
    [L]="Crypto"                    [M]="SQL & command injection"
    [N]="UB / concurrency"          [O]="Performance"
    [P]="API hygiene"
)
ALL_SECTIONS=(A B C D E F G H I J L M N O P)

# ---- resolve requested sections -----------------------------------------
if [ -z "$ONLY_SECTIONS" ]; then
    SECTIONS=("${ALL_SECTIONS[@]}")
else
    IFS=', ' read -r -a _req <<< "$ONLY_SECTIONS"
    SECTIONS=()
    for s in "${_req[@]}"; do
        [ -n "$s" ] || continue
        s=${s^^}
        [ -n "${SECTION_DESC[$s]+x}" ] || die "unknown section: $s (valid: ${ALL_SECTIONS[*]})" 2
        SECTIONS+=("$s")
    done
    [ "${#SECTIONS[@]}" -gt 0 ] || die "--section given but no section named" 2
fi

# Sections that should be hard zero in module code (strict mode).
DEFAULT_STRICT=(A B C L M N O)
if [ -n "$STRICT_OVERRIDE" ]; then
    if [ "${STRICT_OVERRIDE^^}" = "ALL" ]; then
        STRICT_SECTIONS=("${SECTIONS[@]}")
    else
        IFS=', ' read -r -a _ss <<< "$STRICT_OVERRIDE"
        STRICT_SECTIONS=()
        for s in "${_ss[@]}"; do
            [ -n "$s" ] || continue
            s=${s^^}
            [ -n "${SECTION_DESC[$s]+x}" ] || die "unknown strict section: $s" 2
            STRICT_SECTIONS+=("$s")
        done
    fi
else
    STRICT_SECTIONS=("${DEFAULT_STRICT[@]}")
fi

# Sections that actually gate this run = requested ∩ strict.
GATING=()
for s in "${SECTIONS[@]}"; do
    for t in "${STRICT_SECTIONS[@]}"; do
        [ "$s" = "$t" ] && { GATING+=("$s"); break; }
    done
done

# ---- build the pattern spec ---------------------------------------------
: > "$PATSPEC"
emit_patterns() {
    local label=$1 mode=$2 start=$3; shift 3
    local idx=$start p
    for p in "$@"; do
        printf '%s\t%d\t%s\t%s\n' "$label" "$idx" "$mode" "$p" >> "$PATSPEC"
        idx=$((idx + 1))
    done
    printf '%d' "$idx"
}
for s in "${SECTIONS[@]}"; do
    declare -n _arr="$s"
    next=$(emit_patterns "$s" L 0 "${_arr[@]}")
    unset -n _arr
    if declare -p "${s}_ML" >/dev/null 2>&1; then
        declare -n _mlarr="${s}_ML"
        emit_patterns "$s" M "$next" "${_mlarr[@]}" >/dev/null
        unset -n _mlarr
    fi
done

# ---- the matching engine (perl: PCRE + Rust-aware comment stripping) ------
cat > "$PERL_SRC" <<'PERL_EOF'
#!/usr/bin/env perl
use strict; use warnings;

my ($listfile, $specfile, $show) = @ARGV;

# --- load NUL-delimited file list ---
open(my $lf, '<:raw', $listfile) or die "audit: cannot open $listfile: $!\n";
my $blob = do { local $/; <$lf> }; close $lf;
my @files = grep { length } split(/\0/, $blob);

# --- load pattern spec: section \t idx \t mode \t pattern ---
my (@spec, %sections);
open(my $sf, '<:encoding(UTF-8)', $specfile) or die "audit: cannot open $specfile: $!\n";
while (my $l = <$sf>) {
    chomp $l;
    next unless length $l;
    my ($sec, $idx, $mode, $pat) = split(/\t/, $l, 4);
    my $re = eval { qr/$pat/ };
    if (!$re) {
        my $err = $@ // 'unknown error'; $err =~ s/\s+/ /g;
        print "E\tsection $sec pattern #$idx is not a valid regex: $err\n";
        next;
    }
    push @spec, { sec => $sec, idx => $idx, mode => $mode, pat => $pat, re => $re };
    $sections{$sec} = 1;
}
close $sf;

# --- Rust-aware cleaner -------------------------------------------------
# Removes line comments and block comments (nested supported) while keeping
# line numbering intact and leaving string/char literals untouched, so that
# "http://x" and "// not a comment" inside a literal still match.
sub strip_comments {
    my ($s) = @_;
    $s =~ s{
        (?<raw>   r (?<h>\#*) " .*? " \g{h} )
      | (?<str>   " (?: \\. | [^"\\] )* " )
      | (?<chr>   ' (?: \\. | [^'\\] ) ' )
      | (?<line>  // [^\n]* )
      | (?<blk>   (?<bc> /\* (?: [^*/] | \*(?!/) | /(?!\*) | (?&bc) )* \*/ ) )
    }{
        defined $+{line} ? ''
      : defined $+{blk}  ? ("\n" x (($+{blk} =~ tr/\n//)))
      : $&
    }gexs;
    return $s;
}

# --- scan ---------------------------------------------------------------
my (%pat_lines, %pat_matches, %sec_lines, %sec_matches, %hits);
my $nfiles = 0; my $nlines = 0; my @errors;

for my $f (@files) {
    my $fh;
    unless (open($fh, '<:encoding(UTF-8)', $f)) {
        push @errors, "cannot read $f: $!";
        next;
    }
    my $src = do { local $/; <$fh> };
    close $fh;
    next unless defined $src;
    $nfiles++;

    # Honour `// audit-allow:` waivers BEFORE comments are stripped.
    my @raw = split(/\n/, $src, -1);
    for my $i (0 .. $#raw) { $raw[$i] = '' if index($raw[$i], '// audit-allow:') >= 0; }
    my $text = strip_comments(join("\n", @raw));

    my @lines = split(/\n/, $text, -1);
    pop @lines while @lines && $lines[-1] eq '';
    $nlines += scalar @lines;

    for my $p (@spec) {
        my $key = "$p->{sec}\t$p->{idx}";
        if ($p->{mode} eq 'M') {
            my $n = () = ($text =~ /$p->{re}/g);
            next unless $n;
            $pat_lines{$key}   += $n;
            $pat_matches{$key} += $n;
            $sec_matches{$p->{sec}} += $n;
            # locate each match's starting line for dedup + --show
            while ($text =~ /$p->{re}/g) {
                my $pre = substr($text, 0, $-[0]);
                my $ln = 1 + ($pre =~ tr/\n//);
                $sec_lines{$p->{sec}}{"$f:$ln"} = 1;
                push @{$hits{$key}}, "$f:$ln" if $show;
            }
        } else {
            for my $i (0 .. $#lines) {
                my $n = () = ($lines[$i] =~ /$p->{re}/g);
                next unless $n;
                $pat_lines{$key}++;
                $pat_matches{$key} += $n;
                $sec_matches{$p->{sec}} += $n;
                $sec_lines{$p->{sec}}{"$f:" . ($i + 1)} = 1;
                push @{$hits{$key}}, "$f:" . ($i + 1) if $show;
            }
        }
    }
}

# --- emit TSV -----------------------------------------------------------
print "F\t$nfiles\t$nlines\n";
print "E\t$_\n" for @errors;
for my $p (@spec) {
    my $key = "$p->{sec}\t$p->{idx}";
    printf "P\t%s\t%d\t%s\t%d\t%d\t%s\n",
        $p->{sec}, $p->{idx}, $p->{mode},
        ($pat_lines{$key}   // 0),
        ($pat_matches{$key} // 0),
        $p->{pat};
    if ($show && $pat_lines{$key}) {
        print "H\t$p->{sec}\t$p->{idx}\t$_\n" for @{$hits{$key}};
    }
}
for my $sec (sort keys %sections) {
    printf "S\t%s\t%d\t%d\n", $sec,
        scalar(keys %{ $sec_lines{$sec} // {} }),
        ($sec_matches{$sec} // 0);
}
PERL_EOF

RESULTS="$TMPDIR_AUDIT/results.tsv"
if ! perl "$PERL_SRC" "$FILELIST_NUL" "$PATSPEC" "$SHOW" > "$RESULTS"; then
    die "pattern scan failed (perl exited $?)"
fi

# ---- report --------------------------------------------------------------
declare -A SEC_LINES SEC_MATCHES
SCAN_ERRORS=()
SCANNED_FILES=0
SCANNED_LINES=0

while IFS=$'\t' read -r kind f1 f2 f3 f4 f5 f6; do
    case "$kind" in
        F) SCANNED_FILES=$f1; SCANNED_LINES=$f2 ;;
        E) SCAN_ERRORS+=("$f1") ;;
        S) SEC_LINES[$f1]=$f2; SEC_MATCHES[$f1]=$f3 ;;
    esac
done < "$RESULTS"

echo "============================================================"
echo "  RUSTSPLOIT BAD-PATTERN AUDIT"
printf "  %d file(s), %d line(s) under audit\n" "$SCANNED_FILES" "$SCANNED_LINES"
[ "$MISSING" -gt 0 ] && printf "  %d listed path(s) missing and skipped\n" "$MISSING"
echo "============================================================"
echo

if [ "${#SCAN_ERRORS[@]}" -gt 0 ]; then
    echo "  SCAN ERRORS (results below are incomplete):"
    printf '    %s\n' "${SCAN_ERRORS[@]}"
    echo
fi

GRAND_PATTERNS=0
GRAND_PATTERNS_HIT=0
GRAND_LINES=0
GRAND_MATCHES=0
STRICT_HITS=0

for s in "${SECTIONS[@]}"; do
    pats=0; hitp=0
    lines_out=()
    while IFS=$'\t' read -r kind sec idx mode plines pmatches pat; do
        [ "$kind" = "P" ] || continue
        [ "$sec" = "$s" ] || continue
        pats=$((pats + 1))
        if [ "$plines" -gt 0 ]; then
            hitp=$((hitp + 1))
            if [ "$plines" = "$pmatches" ]; then
                lines_out+=("    [$plines] /$pat/")
            else
                lines_out+=("    [$plines lines, $pmatches matches] /$pat/")
            fi
            if [ "$SHOW" = "1" ]; then
                while IFS=$'\t' read -r hk hsec hidx hloc; do
                    [ "$hk" = "H" ] && [ "$hsec" = "$s" ] && [ "$hidx" = "$idx" ] \
                        && lines_out+=("         $hloc")
                done < "$RESULTS"
            fi
        fi
    done < "$RESULTS"

    slines=${SEC_LINES[$s]:-0}
    smatches=${SEC_MATCHES[$s]:-0}
    GRAND_PATTERNS=$((GRAND_PATTERNS + pats))
    GRAND_PATTERNS_HIT=$((GRAND_PATTERNS_HIT + hitp))
    GRAND_LINES=$((GRAND_LINES + slines))
    GRAND_MATCHES=$((GRAND_MATCHES + smatches))

    gate=""
    for t in "${GATING[@]}"; do [ "$t" = "$s" ] && { gate=" *"; STRICT_HITS=$((STRICT_HITS + slines)); break; }; done

    printf "  %-2s %-32s : %5d line(s), %5d match(es) across %2d/%-2d patterns%s\n" \
        "$s" "${SECTION_DESC[$s]}" "$slines" "$smatches" "$hitp" "$pats" "$gate"
    [ "${#lines_out[@]}" -gt 0 ] && printf "%s\n" "${lines_out[@]}"
    echo
done

echo "============================================================"
echo "  TOTALS"
echo "============================================================"
printf "  Patterns scanned       : %d\n" "$GRAND_PATTERNS"
printf "  Patterns with hits     : %d\n" "$GRAND_PATTERNS_HIT"
printf "  Distinct hit lines     : %d  (deduped within each section)\n" "$GRAND_LINES"
printf "  Raw pattern matches    : %d  (overlapping patterns counted separately)\n" "$GRAND_MATCHES"

if [ "${#GATING[@]}" -eq 0 ]; then
    printf "  Gating sections (*)    : none\n"
else
    printf "  Gating sections (*)    : %s\n" "${GATING[*]}"
fi
printf "  Gated hit lines        : %d\n" "$STRICT_HITS"

if [ "${#SCAN_ERRORS[@]}" -gt 0 ]; then
    echo "  RESULT                 : ERROR — $((${#SCAN_ERRORS[@]})) file(s) could not be scanned"
    exit 3
fi

if [ "$STRICT" = "1" ]; then
    if [ "${#GATING[@]}" -eq 0 ]; then
        echo "  RESULT                 : ERROR — --strict cannot fail: no requested section is gated"
        echo "                           (use --strict-sections ALL to gate every requested section)"
        exit 3
    fi
    if [ "$STRICT_HITS" -gt 0 ]; then
        echo "  RESULT                 : STRICT FAILURE — fix before merge"
        exit 1
    fi
    echo "  RESULT                 : STRICT PASS — no hits in gating sections"
    exit 0
fi
echo "  RESULT                 : informational only (use --strict to gate)"
exit 0
