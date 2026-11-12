
-- PolynomialRing
R = CC[x, y]
S = R[a_1..a_5, y_(0,0)..z_(1,1), x]
-- QuotientRing
T1 = S/x
T2 = S/ideal (1+x, a_1)
T3 = S/ideal S_*

-- BaseRings 
B1 = T1.BaseRings 


S = ZZ/101[t_0..t_3]
I = ideal(x^2, y)
Q = R/I
M = Q^2
J = ideal(x, y)
N = M/J

