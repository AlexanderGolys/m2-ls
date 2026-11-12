-- Reports the operand classes for which an installed operator method is never
-- called, because the interpreter evaluates the operation itself before it
-- looks up methods.
--
--   M2 --script probe.m2 <bin|pre|post> <operator>
--   M2 --script probe.m2 classes
--
-- For every operand class (pair), a method returning a marker is installed and
-- the operation is evaluated. The marker is the value of a call that reached
-- the method; any other outcome is printed as `operator | classes | outcome`.
-- Run one operator per process: the installations stay in effect.
debug Core

R = QQ[x]
probeLeft = null
probeRight = null

form = scriptCommandLine#1
operatorSpelling = if #scriptCommandLine > 2 then scriptCommandLine#2 else ""

-- (class name, source of a value of that class). The right samples differ from
-- the left ones, so that equal operands never take a shortcut of their own.
leftSamples = {("ZZ", "2"), ("QQ", "1/2"), ("RR", "1.5"), ("RRi", "interval(1.,2.)"),
	("CC", "(1.5+ii)"), ("CCi", "(interval(1.,2.)+interval(1.,2.)*ii)"),
	("Boolean", if operatorSpelling == "or" then
				"false"
			else "true"),
	("Symbol", "(symbol foo)"), ("String", "\"s\""), ("List", "{1}"),
	("HashTable", "(hashTable {})"), ("Nothing", "null"), ("Ring", "QQ"),
	("PolyElt", "R_0"), ("Function", "sin"), ("Matrix", "matrix{{1}}"),
	("Ideal", "ideal(R_0)"), ("Net", "(net \"a\")")}
rightSamples = {("ZZ", "3"), ("QQ", "2/3"), ("RR", "2.5"), ("RRi", "interval(2.,3.)"),
	("CC", "(2.5+2*ii)"), ("CCi", "(interval(2.,3.)+interval(2.,3.)*ii)"),
	("Boolean", "false"), ("Symbol", "(symbol bar)"), ("String", "\"t\""),
	("List", "{2}"), ("HashTable", "(hashTable {1=>1})"), ("Nothing", "null"),
	("Ring", "ZZ"), ("PolyElt", "R_0^2"), ("Function", "cos"),
	("Matrix", "matrix{{2}}"), ("Ideal", "ideal(R_0^2)"), ("Net", "(net \"b\")")}

if form == "classes" then
	exit(print demark(" ", apply(leftSamples, first)); 0)

-- `?` must answer with a comparison symbol; `incomparable` is one that no
-- native comparison of these samples produces.
marker = if operatorSpelling == "?" then value "symbol incomparable" else "MARKER"
operatorSymbol = value("symbol " | operatorSpelling)

evaluated = samples -> apply(samples, sample -> (sample#0, value sample#1))

-- How the interpreter answered after one installation, or null if the call
-- reached the installed method.
outcome = (installation, expression) -> (
	installation();
	result := try value expression
		else "ERROR";
	if result === "ERROR" then
		"error"
	else if result =!= marker then
		"value:" | toString class result
)

report = (classNames, result) ->
	if result =!= null then
	print concatenate(operatorSpelling, " | ", classNames, " | ", result)

leftValues = evaluated leftSamples
rightValues = evaluated rightSamples

(if form == "bin" then
			scan(leftValues, a -> scan(rightValues, b -> (
				probeLeft = a#1;
				probeRight = b#1;
				report(concatenate(a#0, " | ", b#0), outcome(
					() -> installMethod(operatorSymbol, class a#1, class b#1,
						(u, v) -> marker),
					"probeLeft " | operatorSpelling | " probeRight"
				))
			)))
		else scan(leftValues, a -> (
			probeLeft = a#1;
			report(a#0, outcome(
				() -> installMethod(operatorSymbol, class a#1, u -> marker),
				if form == "pre" then
					operatorSpelling | " probeLeft"
				else "probeLeft " | operatorSpelling
			))
		)))
