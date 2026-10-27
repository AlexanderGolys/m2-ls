randomPlanePoints = (delta, R) -> (
    k := ceiling((-3 + sqrt(9.0 + 8*delta))/2);
    eps := delta - binomial(k + 1, 2);
    if k - 2*eps >= 0
    then minors(k - eps,
        random(R^(k + 1 - eps), R^{k - 2*eps:-1, eps:-2})
    )
    else minors(eps,
        random(R^{k + 1 - eps:0, 2*eps - k:-1}, R^{eps:-2})
    )
);
K = ZZ/101;
ideal(gens Ip2 * random(source gens Ip2, R^{-d}))
res(coker b, DegreeLimit=>0, SyzygyLimit=>60, LengthLimit=>3)
