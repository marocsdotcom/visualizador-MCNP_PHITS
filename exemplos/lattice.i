Exemplo de lattice: elementos 5x5 de varetas, um deles girado por TR
c --- universo 1: vareta combustivel ---
1  1 -10.4  -1        u=1 imp:n=1   $ pastilha
2  2 -6.5   1 -2      u=1 imp:n=1   $ revestimento
3  3 -1.0   2         u=1 imp:n=1   $ moderador
c --- universo 2: tubo guia ---
4  3 -1.0   -3        u=2 imp:n=1
5  2 -6.5   3 -4      u=2 imp:n=1
6  3 -1.0   4         u=2 imp:n=1
c --- universo 10: lattice retangular 5x5 ---
10 3 -1.0  -11 12 -13 14  lat=1 u=10 imp:n=1
        fill=-2:2 -2:2 0:0
             1 1 1 1 1
             1 2 1 2 1
             1 1 2 1 1
             1 2 1 2 1
             1 4r
c --- universo 0 ---
20 0       -20              fill=10        imp:n=1
21 0       -21              *fill=10 (9 0 0  30 60 90  120 30 90  90 90 0)
                                           imp:n=1
22 like 20 but trcl=(0 9 0)
30 3 -1.0  -30 #20 #21 #22  imp:n=1
31 0       30               imp:n=0

c --- superficies ---
1   cz 0.41
2   cz 0.48
3   cz 0.56
4   cz 0.61
11  px 0.63
12  px -0.63
13  py 0.63
14  py -0.63
20  rpp -3.15 3.15  -3.15 3.15  -10 10
21  1 rpp -3.15 3.15  -3.15 3.15  -10 10
30  rcc 4.5 4 -12  0 0 24  12

*tr1 9 0 0  30 60 90  120 30 90  90 90 0
m1  92235 0.04  92238 0.96  8016 2
m2  40000 1
m3  1001 2  8016 1
