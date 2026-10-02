# Built-in symbols

The versioned OpenROAD 12.0 catalogue stores factual constant identifiers and numeric equivalents from the [Actian language reference appendix](https://docs.actian.com/openroad/12.0/LangRef/A._System_Constants_and_Keywords.htm). Each entry links to its source table; documentation prose is not copied. Constants without an unambiguous numeric equivalent retain their name and link only.

Context variables link to the language reference's Literals page. Context legality checking is not implemented.

## System classes

The catalogue records 156 class names and 7,768 member declarations (including inherited repetitions) from the public [OpenROAD 12.0 System Reference Summary](https://docs.actian.com/openroad/12.0/SysRefSum/System_Classes.htm). It stores API facts: names, declaration types, named parameter syntax, defining class, access labels and source links. Descriptive documentation prose is not copied. Duplicate table rows are consolidated; four malformed HTML rows were resolved from the same documented inherited signature or their explicit signature text.

Generated read-only source views are produced lazily from these declarations. Inherited members share binding identities. Ambiguous alternatives, repeated placeholders and variadic signatures are marked incomplete and do not receive negative argument checks. The catalogue does not establish access-control enforcement, full default-value semantics, context legality, global system-procedure coverage or compatibility with every runtime version.

Class ancestry was cross-checked against the individual Language Reference class pages, supplying 173 additional inherited declarations omitted by the summary tables. Public summary omissions are not used to diagnose missing members. Absence from this catalogue does not mean an API is invalid.
