toJSON := beforeImportValue -> beforeImportValue
beforeImport = toJSON 1
needsPackage "JSON"
packageResult = toJSON 1
-- [[localValue]] is reused by the function below.
localValue=1
double = value -> value + localValue
result=double(2)
encoded=toJSON(result)
conditional = if result == 3 then (
encoded
) else null
ZZ
toJ
toJSON := afterImportValue -> afterImportValue
afterImport = toJSON 1
reassigned = "a"
stringUse = reassigned
reassigned = 1
integerUse = reassigned
boundary Module := boundaryParameter -> (
    (m(class, ring)) boundaryParameter;
)
editProbe = 1

orderedActions = if not (orderedLeft == orderedRight) then orderedValue else null
flattenElse = if flatA then flatOne else (if flatB then flatTwo else (if flatC then flatThree else flatFour))
flattenThen = if outerCondition then (if innerCondition then innerThen else innerElse) else outerElse
alreadyFlat = if existingA then existingOne else if existingB then existingTwo else existingThree
dropElseNull = if readyElse then valueElse else null
dropBothNull = if member("Flexible", attrStrings) then null else null
negateSimple = if readySimple then null else valueSimple
negateBinary = if binaryLeft < binaryRight then null else binaryValue
negateEquality = if equalLeft == equalRight then null else equalValue
negateStrict = if strictLeft === strictRight then null else strictValue
cancelConditionNot = if not negatedReady then null else negatedValue
ambiguousMember = memberValue.3
rawGood = "a\nb\tc\""
rawShort = "a\nb"
rawUnsupported = "\101\102\103"
try tryEcho then tryEcho;
try tryValue then tryResult else null;
try bareTryValue else null;
try exceptValue except err do null;
simplifyEqual = if not (simpleLeft == simpleRight) then simpleValue
simplifyUnequal = if not (unequalLeft != unequalRight) then unequalThen else unequalElse
simplifyLess = if not (lessLeft < lessRight) then lessValue
simplifyDoubleNot = if not not doubleNotValue then doubleNotResult
keepSimpleIf = if simpleCondition then simpleResult
rawDelimiter = "a\/\/\/b"
