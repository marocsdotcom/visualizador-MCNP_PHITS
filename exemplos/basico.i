Exemplo basico: superficies, macrobodies, uniao e complemento
c --- celulas ---
1  1 -7.86  -1                    imp:n=1   $ esfera de aco
2  2 -1.0   -2 1                  imp:n=1   $ casca de agua
3  3 -11.35 -10                   imp:n=1   $ cilindro de chumbo (RCC)
4  4 -2.7   -11                   imp:n=1   $ caixa de aluminio (RPP)
5  1 -7.86  -12 13                imp:n=1   $ tronco de cone oco
6  5 -8.9   -14                   imp:n=1   $ toro de cobre
7  2 -1.0   (-15 : -16) 17        imp:n=1   $ uniao de dois cilindros, furada
8  6 -1.2e-3 -99 #1 #2 #3 #4 #5 #6 #7
                                  imp:n=1   $ ar
9  0        99                    imp:n=0   $ mundo externo

c --- superficies ---
1   so  4
2   so  6
10  rcc 12 0 -5  0 0 10  3
11  rpp -16 -10  -4 4  -3 3
12  trc 0 14 -5  0 0 10  4 1.5
13  c/z 0 14 0.8
14  tz  0 -14 0  4 1 1.5
15  rcc 4 -12 10   16 0 0  1.5
16  rcc 12 -20 10  0 16 0  1.5
17  s   12 -12 10 1.0
99  so  30

m1  26000 1
m2  1001 2  8016 1
m3  82000 1
m4  13027 1
m5  29000 1
m6  7014 0.8  8016 0.2
