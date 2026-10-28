use crate::geom::{Model, Prim, Transform};
use crate::{mcnp, phits};
use glam::{DMat3, DVec3};

fn cell_at(m: &Model, x: f64, y: f64, z: f64) -> Option<i32> {
    m.locate(DVec3::new(x, y, z)).map(|(f, _)| m.cells[f.leaf].id)
}

fn no_warnings(m: &Model) {
    assert!(m.warnings.is_empty(), "avisos inesperados: {:#?}", m.warnings);
}

#[test]
fn basic_surfaces_and_macrobodies() {
    let m = mcnp::parse(include_str!("../exemplos/basico.i"));
    no_warnings(&m);
    assert_eq!(m.cells.len(), 9);
    assert_eq!(cell_at(&m, 0.0, 0.0, 0.0), Some(1));
    assert_eq!(cell_at(&m, 5.0, 0.0, 0.0), Some(2));
    assert_eq!(cell_at(&m, 12.0, 0.0, 0.0), Some(3));
    assert_eq!(cell_at(&m, 12.0, 0.0, 5.5), Some(8), "acima da tampa do RCC");
    assert_eq!(cell_at(&m, -13.0, 0.0, 0.0), Some(4));
    assert_eq!(cell_at(&m, 0.0, 14.0, 0.0), Some(8), "furo do tronco de cone");
    assert_eq!(cell_at(&m, 2.0, 14.0, 0.0), Some(5));
    assert_eq!(cell_at(&m, 3.5, 14.0, 4.0), Some(8), "fora do cone, que estreita em +z");
    assert_eq!(cell_at(&m, 4.0, -14.0, 0.0), Some(6), "toro");
    assert_eq!(cell_at(&m, 0.0, -14.0, 0.0), Some(8), "centro do toro");
    assert_eq!(cell_at(&m, 8.0, -12.0, 10.0), Some(7));
    assert_eq!(cell_at(&m, 12.0, -18.0, 10.0), Some(7));
    assert_eq!(cell_at(&m, 12.0, -12.0, 10.0), Some(8), "esfera removida da união");
    assert_eq!(cell_at(&m, 25.0, 0.0, 0.0), Some(8));
    assert_eq!(cell_at(&m, 40.0, 0.0, 0.0), Some(9));
    assert!(m.cells[m.cell_index[&9]].outside);
    assert_eq!(m.materials.len(), 6);
}

#[test]
fn rectangular_lattice_with_transforms() {
    let m = mcnp::parse(include_str!("../exemplos/lattice.i"));
    no_warnings(&m);
    assert_eq!(cell_at(&m, 0.0, 0.0, 0.0), Some(4), "tubo guia central");
    assert_eq!(cell_at(&m, 1.26, 0.0, 0.0), Some(1), "vareta em [1,0]");
    assert_eq!(cell_at(&m, 1.26 + 0.45, 0.0, 0.0), Some(2), "revestimento");
    assert_eq!(cell_at(&m, 1.26, 1.26, 3.0), Some(4), "tubo guia em [1,1]");
    assert_eq!(cell_at(&m, -2.52, -2.52, 0.0), Some(1), "canto [-2,-2]");
    assert_eq!(cell_at(&m, 1.26 + 0.6, 0.6, 0.0), Some(3), "moderador no canto do elemento");
    assert_eq!(cell_at(&m, 0.6, 0.6, 0.0), Some(6), "água fora do tubo guia");
    assert_eq!(cell_at(&m, 0.0, 9.0, 0.0), Some(4), "cópia deslocada por TRCL");
    assert_eq!(cell_at(&m, 1.26, 9.0, 0.0), Some(1));
    assert_eq!(cell_at(&m, 9.0, 0.0, 0.0), Some(4), "cópia girada por *FILL");
    // Na cópia girada o eixo x' aponta para (cos 30°, sen 30°).
    let (c, s) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    assert_eq!(cell_at(&m, 9.0 + 1.26 * c, 1.26 * s, 0.0), Some(1));
    assert_eq!(cell_at(&m, 9.0 + 1.26, 0.0, 0.0), Some(3));
    assert_eq!(cell_at(&m, 4.5, 4.0, 11.0), Some(30));
    assert_eq!(cell_at(&m, 4.5, 4.0, 13.0), Some(31));
    // `#22` tem que acompanhar o TRCL da célula 22, senão a 30 se sobrepõe a ela.
    let c30 = &m.cells[m.cell_index[&30]];
    assert!(!c30.node.contains(&m.prims, DVec3::new(0.0, 9.0, 0.0)));
    assert!(c30.node.contains(&m.prims, DVec3::new(0.0, 5.0, 0.0)));
    let (_, levels) = m.locate(DVec3::new(1.26, -1.26, 0.0)).unwrap();
    assert_eq!(m.describe(&levels), "20 > 10[1 -1 0] > 4");
}

#[test]
fn hexagonal_lattice() {
    let m = mcnp::parse(include_str!("../exemplos/hex.i"));
    no_warnings(&m);
    assert_eq!(cell_at(&m, 0.0, 0.0, 0.0), Some(4), "posição central sem vareta");
    assert_eq!(cell_at(&m, 1.4, 0.0, 0.0), Some(1));
    assert_eq!(cell_at(&m, -1.4, 0.0, 0.0), Some(1));
    // Vizinho na direção s (60° de r).
    assert_eq!(cell_at(&m, 0.7, 1.4 * 60f64.to_radians().sin(), 0.0), Some(1));
    assert_eq!(cell_at(&m, 0.7 + 0.48, 1.4 * 60f64.to_radians().sin(), 0.0), Some(2));
    assert_eq!(cell_at(&m, 4.3, 0.0, 0.0), Some(21));
    assert_eq!(cell_at(&m, 6.0, 0.0, 0.0), Some(22));
}

#[test]
fn phits_sections_variables_and_transform() {
    let src = include_str!("../exemplos/phits_basico.inp");
    assert!(phits::looks_like_phits(src));
    assert!(!phits::looks_like_phits(include_str!("../exemplos/basico.i")));
    let m = phits::parse(src, None);
    no_warnings(&m);
    assert_eq!(m.cells.len(), 7);
    assert_eq!(cell_at(&m, -6.0, 0.0, 0.0), Some(1));
    assert_eq!(cell_at(&m, 6.0, 0.0, 0.0), Some(2));
    assert_eq!(cell_at(&m, 6.0 + 4.5, 0.0, 0.0), Some(4), "raio do RCC é c2-1 = 4");
    assert_eq!(cell_at(&m, 0.0, -9.0, 0.0), Some(3));
    assert_eq!(cell_at(&m, 3.0, -8.0, 0.0), Some(3), "inserto girado 20°");
    assert_eq!(cell_at(&m, 3.0, -10.0, 0.0), Some(4));
    assert_eq!(cell_at(&m, 14.9, 14.9, 14.9), Some(4));
    assert_eq!(cell_at(&m, 0.0, 22.0, 0.0), Some(5));
    assert_eq!(cell_at(&m, 0.0, 17.0, 0.0), Some(6));
    assert_eq!(cell_at(&m, 100.0, 0.0, 0.0), Some(9));
    assert!(m.cells[m.cell_index[&9]].outside);
    assert_eq!(m.materials.len(), 4);
}

#[test]
fn card_syntax_edge_cases() {
    let src = "titulo
1 1 -1.0 -1 2 imp:n=1 $ comentario
c comentario no meio
2 0 #1 -3 &
   imp:n=1
3 0 3 imp:n=0

1 so 5
2 px -1.0+1
3 so 1.0d1

tr4 0 0 0
m1 1001.80c 2 8016.80c 1 nlib=80c
";
    let m = mcnp::parse(src);
    no_warnings(&m);
    assert_eq!(m.cells.len(), 3);
    assert_eq!(cell_at(&m, 0.0, 0.0, 0.0), Some(1));
    assert_eq!(cell_at(&m, -11.0, 0.0, 0.0), Some(3));
    assert_eq!(cell_at(&m, -8.0, 0.0, 0.0), Some(2));
    assert_eq!(m.materials[&1].nuclides.len(), 2);
}

#[test]
fn reports_problems_instead_of_failing() {
    let m = mcnp::parse("t\n1 0 -1 -7\n2 0 1 fill=9\n\n1 so 5\n3 xyz 1\n");
    assert_eq!(m.cells.len(), 2);
    assert_eq!(m.warnings.len(), 3, "{:#?}", m.warnings);
}

#[test]
fn transformed_quadric_matches_point_transform() {
    let (a, b) = (0.6f64, 0.3f64);
    let rot = DMat3::from_rotation_z(a) * DMat3::from_rotation_x(b);
    let tr = Transform { rot, off: DVec3::new(1.0, -2.0, 3.0) };
    let m = mcnp::parse("t\n1 0 -1\n\n1 gq 1 2 3 0.4 0.5 0.6 0.7 0.8 0.9 -10\n");
    let q = &m.prims[0];
    let moved = q.transformed(&tr);
    for p in [DVec3::new(0.3, 1.0, -2.0), DVec3::new(-4.0, 2.5, 0.1), DVec3::ZERO] {
        assert!((moved.eval(p) - q.eval(tr.to_local(p))).abs() < 1e-9);
    }
}

#[test]
fn ray_hits_match_surface_zero() {
    let m = mcnp::parse(include_str!("../exemplos/basico.i"));
    let (o, d) = (DVec3::new(-28.0, 0.3, 0.2), DVec3::new(1.0, 0.01, 0.02).normalize());
    let mut hits = 0;
    for p in &m.prims {
        if let Some(t) = p.intersect(o, d, 1e-9) {
            let scale = match p {
                Prim::Plane { .. } => 1.0,
                _ => 100.0,
            };
            assert!(p.eval(o + d * t).abs() < 1e-7 * scale, "{:?}", p);
            hits += 1;
        }
    }
    assert!(hits > 5);
}

/// O avanço de fronteira em fronteira (com o cache de vizinhas e a contagem
/// incremental) tem que concordar com a localização ponto a ponto.
#[test]
fn ray_walk_matches_point_location() {
    use crate::geom::{Hint, Work};
    let models = [
        mcnp::parse(include_str!("../exemplos/basico.i")),
        mcnp::parse(include_str!("../exemplos/lattice.i")),
        mcnp::parse(include_str!("../exemplos/hex.i")),
        phits::parse(include_str!("../exemplos/phits_basico.inp"), None),
    ];
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    for m in &models {
        let (lo, hi) = m.bbox;
        let (mut hint, mut levels, mut work) = (Hint::default(), Vec::new(), Work::default());
        let mut segments = 0;
        for _ in 0..300 {
            let mut point = || lo + (hi - lo) * DVec3::new(rnd(), rnd(), rnd());
            let (o, target) = (point(), point());
            let d = (target - o).normalize();
            let mut t = 0.0;
            for _ in 0..200 {
                let Some(found) = m.descend(o + d * t, d, &mut hint, &mut levels) else { break };
                assert_eq!(Some(found), m.locate(o + d * t).map(|(f, _)| f), "cache de vizinhas");
                let Some((dt, _)) = m.next_boundary(&levels, &mut work) else { break };
                for k in 1..8 {
                    let s = t + dt * k as f64 / 8.0;
                    let here = m.locate(o + d * s).map(|(f, _)| f);
                    assert_eq!(here, Some(found), "{}: saiu da célula antes da fronteira (t={s}, de {t} a {})", m.title, t + dt);
                }
                segments += 1;
                t += dt + 1e-8 * (1.0 + t);
            }
        }
        assert!(segments > 600, "{}: só {} trechos", m.title, segments);
    }
}

#[test]
fn meshtal_column_format() {
    let src = "mcnp   version 6     ld=02/20/18  probid =  01/01/26 10:00:00
 teste
 Number of histories used for normalizing tallies =      1000000.00

 Mesh Tally Number        14
 This is a photon mesh tally.

 Tally bin boundaries:
    X direction:     -1.00      0.00
          1.00
    Y direction:      0.00      2.00
    Z direction:      0.00      1.00      2.00
    Energy bin boundaries: 0.00E+00 1.00E+00 1.00E+36

   Energy         X         Y         Z     Result     Rel Error
  1.000E+00    -0.500     1.000     0.500 1.00000E-03 1.00000E-01
  1.000E+00     0.500     1.000     0.500 2.00000E-03 1.00000E-01
  1.000E+00    -0.500     1.000     1.500 3.00000E-03 1.00000E-01
  1.000E+00     0.500     1.000     1.500 4.00000E-03 1.00000E-01
  1.000E+36    -0.500     1.000     0.500 1.00000E-03 1.00000E-01
   Total       -0.500     1.000     0.500 5.00000E-03 2.00000E-02
   Total        0.500     1.000     0.500 6.00000E-03 3.00000E-02
   Total       -0.500     1.000     1.500 7.00000E-03 4.00000E-02
   Total        0.500     1.000     1.500 8.00000E-03 5.00000E-02

 Mesh Tally Number        24
 This is a photon mesh tally.

 Tally bin boundaries:
    X direction:      0.00      1.00      2.00
    Y direction:      0.00      1.00
    Z direction:      0.00      1.00

         X         Y         Z     Result     Rel Error
     0.500     0.500     0.500 1.50000E+00 1.00000E-02
     1.500     0.500     0.500 0.00000E+00 0.00000E+00
";
    let meshes = crate::mesh::parse_meshtal(src).unwrap();
    assert_eq!(meshes.len(), 2);
    let m = &meshes[0];
    assert_eq!(m.name, "FMESH 14");
    assert_eq!(m.edges[0], vec![-1.0, 0.0, 1.0], "fronteiras continuam na linha seguinte");
    assert_eq!(m.at(DVec3::new(-0.3, 0.1, 0.9)), Some((5e-3, 0.02)), "a linha Total vale mais que a soma");
    assert_eq!(m.at(DVec3::new(0.9, 1.9, 1.1)), Some((8e-3, 0.05)));
    assert_eq!(m.at(DVec3::new(1.1, 1.0, 1.0)), None, "fora da malha");
    assert_eq!(meshes[1].at(DVec3::new(0.2, 0.2, 0.2)), Some((1.5, 0.01)), "sem coluna de energia");
    assert!(crate::mesh::parse_meshtal("qualquer coisa").is_err());
}

#[test]
fn lost_particles_from_output() {
    let src = "1   lost particle no.  1    no cell found in subroutine track     history no.   4321

 the photon currently being tracked has reached surface   12.  there
 appears to be no cell on the other side of the surface from cell   3

 x,y,z coordinates:   1.50000E+00 -2.00000E+00  3.25000E+01
 u,v,w direction cosines:   0.00000E+00  6.00000E-01  8.00000E-01
 energy =  1.0E+00
 x,y,z coordinates:   9.0E+00  9.0E+00  9.0E+00
";
    let lost = mcnp::lost_particles(src);
    assert_eq!(lost, vec![(DVec3::new(1.5, -2.0, 32.5), DVec3::new(0.0, 0.6, 0.8))]);
    assert!(mcnp::lost_particles("lost 10 10").is_empty());
}

#[test]
fn mctal_lattice_tally_follows_the_f_card() {
    // Três detectores (célula 1 no lattice 2) dentro da caixa 3.
    let input = "tally em lattice
1 1 -1.0 -1 u=1 imp:p=1
2 0 -2 lat=1 fill=1 u=2 imp:p=1
3 0 -3 fill=2 imp:p=1
4 0 3 imp:p=0

1 rpp -1 1 -1 1 -1 1
2 rpp -0.5 0.5 -5 5 -5 5
3 rpp -1.5 1.5 -1 1 -1 1

m1 1001 2 8016 1
fc6 perfil
f6:p (1<2[-1:1 0:0 0:0])
f16:p 3 t
";
    let m = mcnp::parse(input);
    no_warnings(&m);
    assert_eq!(m.tallies.len(), 2);
    assert_eq!(m.tallies[0].comment, "perfil");
    assert_eq!(m.tallies[0].bins.as_ref().unwrap()[2], vec![(1, None), (2, Some([1, 0, 0]))]);
    let mctal = "mcnp       6     01/01/26 10:00:00     1     1000     123
 tally em lattice
ntal     2
    6   16
tally    6                   -1    0    0
 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
     perfil
f        3
      1      1      1
d        1
u        0
s        0
m        0
c        0
e        0
t        0
vals
1.00000E-01  0.0100  2.00000E-01  0.0200
3.00000E-01  0.0300
tfc    1       1       1       1       1       1       1       1       1
       1000  1.0E-01  1.0E-02  0.0E+00
tally   16                   -1    0    0
 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
f        2
      3      0
d        1
u        0
s        0
m        0
c        0
et       2
  1.0 2.0
t        0
vals
5.0E+00 0.5 7.0E+00 0.1 6.0E+00 0.5 8.0E+00 0.2
tfc    1       1       1       1       1       1       1       1       1
";
    let fields = crate::mesh::load_results(mctal, "mctal", &m).unwrap();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name(), "F6 perfil");
    let at = |f: &crate::mesh::Field, x: f64| {
        let p = DVec3::new(x, 0.0, 0.0);
        f.at(p, &m.locate(p).unwrap().1)
    };
    assert_eq!(at(&fields[0], -1.0), Some((0.1, 0.01)), "elemento [-1 0 0]");
    assert_eq!(at(&fields[0], 0.2), Some((0.2, 0.02)));
    assert_eq!(at(&fields[0], 1.3), Some((0.3, 0.03)));
    assert_eq!(at(&fields[1], 0.0), Some((7.0, 0.1)), "célula 3 inteira, total em energia");
    assert_eq!(fields[0].positive_range(), Some((0.1, 0.3)));
}

#[test]
fn phits_xyz_tally_output() {
    let out = "[ T-Track ]
    title = [t-track] in xyz mesh
     mesh =  xyz            # mesh type is xyz scoring mesh
   x-type =    2            # x-mesh is linear given by xmin, xmax and nx
     xmin =  -1.000000      # minimum value of x-mesh points
       nx =      1          # number of x-mesh points
#    data = ( x(i), i = 1, nx + 1 )
#          -1.00000E+00  1.00000E+00
   y-type =    2            # y-mesh is linear given by ymin, ymax and ny
       ny =      1          # number of y-mesh points
#    data = ( y(i), i = 1, ny + 1 )
#          -1.00000E+00  1.00000E+00
   z-type =    1            # z-mesh is given by the below data
       nz =      3          # number of z-mesh points
#    data = ( z(i), i = 1, nz + 1 )
            0.00000E+00  1.00000E+00  3.00000E+00
            6.00000E+00
   e-type =    2            # e-mesh is linear given by emin, emax and ne
       ne =      1          # number of e-mesh points
#    data = ( e(i), i = 1, ne + 1 )
#           0.00000E+00  2.00000E+01
     axis =       z            # axis of output
     file = pdd.out   # file name of output for the above axis

#newpage:
#   no. =  1   ie  =  1   ix  =  1   iy  =  1
#   e = (   0.0000E+00  -   2.0000E+01  )

x: z[cm]
h: n            x            y(photon  ),hh0l n
#  z-lower      z-upper        photon    r.err
   0.0000E+00   1.0000E+00   2.0000E-06  0.0100
   1.0000E+00   3.0000E+00   4.0000E-06  0.0200
   3.0000E+00   6.0000E+00   1.0000E-06  0.0300
#   sum over    6.0000E+00   4.0839E-07  0.0059
";
    let m = mcnp::parse(include_str!("../exemplos/basico.i"));
    let fields = crate::mesh::load_results(out, "pdd.out", &m).unwrap();
    let f = &fields[0];
    assert_eq!(f.name(), "pdd.out");
    assert_eq!(f.at(DVec3::new(0.0, 0.0, 2.0), &[]), Some((4e-6, 0.02)));
    assert_eq!(f.at(DVec3::new(0.5, -0.5, 5.9), &[]), Some((1e-6, 0.03)));
    assert_eq!(f.at(DVec3::new(0.0, 0.0, 6.5), &[]), None);
    assert_eq!(f.bounds(), Some((DVec3::new(-1.0, -1.0, 0.0), DVec3::new(1.0, 1.0, 6.0))));
}

#[test]
fn input_echo_from_mcnp6_output() {
    let out = "          Code Name & Version = MCNP_6.20, 6.2.0
         1-       titulo
         2-       1 0 -1 imp:p=1
         3-       2 0 1 imp:p=0
         4-
         5-       1 so 5
 warning.  qualquer coisa
         6-
";
    let input = mcnp::input_from_output(out).unwrap();
    let m = mcnp::parse(&input);
    assert_eq!(m.title, "titulo");
    assert_eq!(cell_at(&m, 0.0, 0.0, 0.0), Some(1));
    assert_eq!(cell_at(&m, 9.0, 0.0, 0.0), Some(2));
}

#[test]
fn diagnosis_catches_input_mistakes() {
    use crate::diag::{input_findings, run_findings, source, Level};
    // Material 2 sem cartão, fonte no mundo externo, tally em célula que não
    // existe e pedindo elétrons com MODE P, e nenhum critério de parada.
    let m = mcnp::parse("erros de proposito
1 1 -1.0 -1 imp:p=1
2 2 -2.0 1 -2 imp:p=1
3 0 2 imp:p=0

1 so 5
2 so 10

mode p
m1 1001 2 8016 -1
sdef pos=50 0 0 erg=1 vec=0 0 1 dir=1
f6:e 1 77
");
    let found = input_findings(&m);
    let has = |level: Level, what: &str| found.iter().any(|f| f.level == level && f.text.contains(what));
    assert!(has(Level::Error, "sem cartão de material: 2"), "{:#?}", found);
    assert!(has(Level::Error, "mistura frações"));
    assert!(has(Level::Error, "mundo externo"));
    assert!(has(Level::Error, "células que não existem: 77"));
    assert!(has(Level::Error, "não está no MODE"));
    assert!(has(Level::Warn, "critério de parada"));
    let src = source(&m).unwrap();
    assert_eq!((src.pos, src.dir), (DVec3::new(50.0, 0.0, 0.0), Some(DVec3::Z)));

    // Um input correto não levanta erro nem aviso.
    let ok = mcnp::parse("certo\n1 1 -1.0 -1 imp:p=1\n2 0 1 imp:p=0\n\n1 so 5\n\nmode p\nm1 1001 2 8016 1\nsdef pos=0 0 0\nf6:p 1\nnps 1000\n");
    let found = input_findings(&ok);
    assert!(found.iter().all(|f| f.level < Level::Warn), "{:#?}", found);

    let phits = phits::parse(include_str!("../exemplos/phits_basico.inp"), None);
    assert!(input_findings(&phits).iter().all(|f| f.level != Level::Error));

    let out = "1mcnp     version 6
 warning.  random number period exceeded.  decrease stride.
           results of 10 statistical checks for the estimated answer for the tally fluctuation chart (tfc) bin of tally        8
 passed?        yes          yes      no
 warning.  the tally in the tally fluctuation chart bin did not pass  3 of the 10 statistical checks.
           results of 10 statistical checks for the estimated answer for the tally fluctuation chart (tfc) bin of tally        6
 passed?        yes          yes      yes
 dump no.    2 on file runtpe     nps =     5000     coll =  1  ctm = 1.0
";
    let run = run_findings(out).unwrap();
    let has = |level: Level, what: &str| run.iter().any(|f| f.level == level && f.text.contains(what));
    assert!(has(Level::Warn, "tally 8: não passou em 3"), "{:#?}", run);
    assert!(has(Level::Ok, "tally 6: passou"));
    assert!(has(Level::Warn, "nps = 5000"), "rodada sem fim registrado");
    assert!(has(Level::Warn, "random number period"));
    assert!(run_findings("um texto qualquer").is_none());
}

#[test]
fn curve_comparison_ratio_and_gamma() {
    use crate::compare::{gamma, ratio, Pair};
    // B = 2·A com 1% de erro em cada: o ajuste acha k = 2 e a razão fica em 1.
    let pairs: Vec<Pair> = (0..20).map(|i| (i as f64 * 0.5, 100.0 - 4.0 * i as f64, 0.01, 2.0 * (100.0 - 4.0 * i as f64), 0.01)).collect();
    let fitted = ratio(&pairs, true).unwrap();
    assert!((fitted.k - 2.0).abs() < 1e-9, "k = {}", fitted.k);
    assert!(fitted.mean_dev < 1e-9 && fitted.chi2 < 1e-9);
    assert_eq!((fitted.within, fitted.with_sigma), (20, 20));
    // Sem o ajuste a razão é 2: desvio de 100%, muito fora de 2σ.
    let raw = ratio(&pairs, false).unwrap();
    assert!((raw.mean_dev - 1.0).abs() < 1e-9 && raw.within == 0);
    assert!((raw.points[3].2 - 2.0 * (2e-4f64).sqrt()).abs() < 1e-9, "incerteza da razão");
    assert!(ratio(&pairs[..1], true).is_none());

    // Gama: B igual a A deslocado 1 mm numa rampa passa com 2%/2 mm…
    let ramp = |d: f64| 100.0 - 5.0 * d;
    let shifted: Vec<Pair> = (0..=20).map(|i| i as f64 * 0.5).map(|d| (d, ramp(d), 0.0, ramp(d - 0.1), 0.0)).collect();
    let g = gamma(&shifted, 1.0, 0.02, 0.2, 0.1).unwrap();
    assert_eq!(g.pass, g.points.len());
    assert!(g.max <= 0.5 + 1e-9, "o deslocamento de 1 mm vale no máximo 0,5: {}", g.max);
    // …e B 10% acima não passa em lugar nenhum da parte alta.
    let high: Vec<Pair> = (0..=20).map(|i| i as f64 * 0.5).map(|d| (d, ramp(d), 0.0, ramp(d) + 10.0, 0.0)).collect();
    let g = gamma(&high, 1.0, 0.02, 0.2, 0.1).unwrap();
    assert_eq!(g.pass, 0, "{:?}", g.points);
    // Abaixo do corte de 10% do máximo os pontos não entram.
    assert!(g.points.iter().all(|p| ramp(p.0) >= 10.0));
}

#[test]
fn input_comparison_lists_what_changed() {
    use crate::compare::{inputs, Kind};
    let old = mcnp::parse("antes
1 1 -1.0 -1 imp:p=1
2 0 1 -2 imp:p=1
3 0 2 imp:p=0

1 so 5
2 so 10

mode p
m1 1001 2 8016 1
nps 1000
");
    let new = mcnp::parse("depois
1 1 -1.2 -1 imp:p=1
2 0 1 -2 imp:p=1
4 0 2 -3 imp:p=1
3 0 3 imp:p=0

1 so 5
2 so 12
3 so 20

mode p
m1 1001 2 8016 1
m2 82000 1
nps 5000
ctme 10
");
    let d = inputs(&old, &new);
    let find = |list: &[crate::compare::Change], what: &str| list.iter().find(|c| c.what == what).map(|c| c.kind);
    assert_eq!(find(&d.cells, "célula 1"), Some(Kind::Changed), "{:#?}", d.cells);
    assert_eq!(find(&d.cells, "célula 2"), None);
    assert_eq!(find(&d.cells, "célula 3"), Some(Kind::Changed));
    assert_eq!(find(&d.cells, "célula 4"), Some(Kind::Added));
    assert_eq!(find(&d.surfaces, "superfície 2"), Some(Kind::Changed));
    assert_eq!(find(&d.surfaces, "superfície 3"), Some(Kind::Added));
    // A superfície alterada leva a uma célula que a usa.
    assert_eq!(d.surfaces.iter().find(|c| c.what == "superfície 2").unwrap().cell, Some(2));
    assert_eq!(find(&d.materials, "m2"), Some(Kind::Added));
    assert_eq!(find(&d.materials, "m1"), None);
    assert_eq!(find(&d.data, "nps"), Some(Kind::Changed));
    assert_eq!(find(&d.data, "ctme"), Some(Kind::Added));
    assert_eq!(find(&d.data, "m2"), None, "material não se repete nos dados");
    assert_eq!(inputs(&old, &old).len(), 0);
    // Importância lida do cartão da célula, para a cor por importância.
    assert_eq!((new.cells[0].imp, new.cells[3].imp), (1.0, 0.0));
}

#[test]
fn control_points_sum_weighted_by_muf() {
    use crate::mesh::{load_results, weighted_sum};
    use std::sync::Arc;
    let m = mcnp::parse("soma
1 1 -1.0 -1 imp:p=1
2 1 -1.0 1 -2 imp:p=1
3 0 2 imp:p=0

1 so 5
2 so 10

m1 1001 2 8016 1
fc6 figado
f6:p 1
f16:p 2
");
    let mctal = |muf: &str, a: &str, b: &str| {
        format!("mcnpx 2.7.0 x\n arco cp gantry=1.0 {}\nntal 2\n 6 16\ntally 6 -1 0\n 0 1\n     figado\nf 1\n 1\nd 1\nvals\n {}\ntally 16 -1 0\n 0 1\nf 1\n 2\nd 1\nvals\n {}\n", muf, a, b)
    };
    let load = |text: &str, name: &str| load_results(text, name, &m).unwrap().into_iter().map(Arc::new).collect::<Vec<_>>();
    let mut fields = load(&mctal("MUf=0.5", "2.0 0.1", "4.0 0.2"), "pasta/cp001.m");
    assert!(weighted_sum(&fields).is_none(), "um arquivo só não tem o que somar");
    fields.extend(load(&mctal("MUf=0.25", "4.0 0.1", "8.0 0.05"), "cp002.m"));
    // O mesmo arquivo de novo não entra em dobro.
    fields.extend(load(&mctal("MUf=0.25", "4.0 0.1", "8.0 0.05"), "cp002.m"));
    let sum = weighted_sum(&fields).unwrap();
    assert_eq!(sum.files, vec![("cp001.m".to_string(), 0.5), ("cp002.m".to_string(), 0.25)]);
    assert!(sum.missing.is_empty());
    let (liver, other) = (&sum.rows[0], &sum.rows[1]);
    assert!(liver.name.contains("figado") && liver.files == 2 && liver.cell == Some(0));
    assert!((liver.value - 2.0).abs() < 1e-12, "0,5·2 + 0,25·4 = {}", liver.value);
    assert!((liver.error - (0.02f64).sqrt() / 2.0).abs() < 1e-12, "{}", liver.error);
    assert!((other.value - 4.0).abs() < 1e-12);
    assert!((other.error - ((0.4f64).powi(2) + (0.1f64).powi(2)).sqrt() / 4.0).abs() < 1e-12);
    // O campo somado pinta cada célula com o seu total.
    let field = sum.field.unwrap();
    assert!(field.is_sum());
    let levels = m.locate(DVec3::new(7.0, 0.0, 0.0)).unwrap().1;
    assert!((field.at(DVec3::ZERO, &levels).unwrap().0 - 4.0).abs() < 1e-12);

    // Sem MUf num dos títulos a soma não é feita, e diz qual arquivo.
    fields.extend(load(&mctal("sem peso", "1.0 0.1", "1.0 0.1"), "cp003.m"));
    let sum = weighted_sum(&fields).unwrap();
    assert_eq!(sum.missing, vec!["cp003.m".to_string()]);
    assert!(sum.rows.is_empty() && sum.field.is_none());
}

#[test]
fn moving_surfaces_rewrites_only_their_cards() {
    use crate::edit::{move_surfaces, move_transform};
    let src = "edicao
1 1 -1.0 -1 -2 3 -4 imp:p=1  $ alvo
2 0 -5 imp:p=1
3 0 -6 imp:p=1
4 0 -7 imp:p=1
5 0 -8 imp:p=1
9 0 -9 #1 #2 #3 #4 #5 imp:p=1
10 0 9 imp:p=0

1 so 3
2 pz 2.5
3 rpp -1 1 -1 1 -1 1
4 cz 1.2  $ furo
5 1 so 1
6 gq 1 1 1 0 0 0 -2 0 0 0.75
7 2 px 0.5
8 kz 1 0.3 1
9 so 100

m1 1001 2 8016 1
tr1 10 0 0
*tr2 0 20 0 90 0 90  0 90 90  90 90 0
";
    let m = mcnp::parse(src);
    no_warnings(&m);
    let d = DVec3::new(0.5, -0.25, 1.0);
    let ids = [1, 2, 3, 4, 5, 6, 7, 8];
    let text = move_surfaces(src, &m, &ids, d).unwrap();
    let n = mcnp::parse(&text);
    no_warnings(&n);
    // Cada superfície movida vale em p + d o que valia em p.
    let probes = [DVec3::new(0.3, -0.7, 0.2), DVec3::new(2.0, 1.0, -1.5), DVec3::new(-0.9, 0.4, 3.1), DVec3::new(10.4, 19.6, 0.3)];
    for id in ids {
        let (a, b) = (&m.surfaces[m.surf_index[&id]], &n.surfaces[n.surf_index[&id]]);
        assert_eq!(a.prims.len(), b.prims.len(), "superfície {}", id);
        for (&pa, &pb) in a.prims.iter().zip(&b.prims) {
            for p in probes {
                let (before, after) = (m.prims[pa as usize].eval(p), n.prims[pb as usize].eval(p + d));
                assert!((before - after).abs() < 1e-7 * (1.0 + before.abs()), "superfície {} em {:?}: {} × {}", id, p, before, after);
            }
        }
    }
    // A que não foi escolhida, as células e os dados ficam iguais; o
    // comentário da superfície continua no cartão, e o anterior fica anotado.
    assert!(text.contains("\n9 so 100\n") && text.contains("1 1 -1.0 -1 -2 3 -4 imp:p=1  $ alvo") && text.contains("*tr2 0 20 0 90"));
    assert!(text.contains("c 4 cz 1.2  $ furo") && text.contains("$ furo"), "{}", text);
    assert!(text.contains("c visualizador: superfície 2 deslocada de (0.5, -0.25, 1) cm. Antes:"));
    assert!(text.contains("\n2 pz 3.5\n"), "pz 2.5 + 1 = 3.5");
    assert!(text.lines().all(|l| l.len() <= 80));

    // Mover a TR1 leva junto a superfície 5, escrita com ela.
    let text = move_transform(src, &m, 1, d).unwrap();
    let n = mcnp::parse(&text);
    assert!(text.contains("tr1 10.5 -0.25 1"), "{}", text);
    let (a, b) = (m.surfaces[m.surf_index[&5]].prims[0], n.surfaces[n.surf_index[&5]].prims[0]);
    for p in probes {
        assert!((m.prims[a as usize].eval(p) - n.prims[b as usize].eval(p + d)).abs() < 1e-9);
    }
    // PX ao longo de X numa superfície cilíndrica do eixo z não serve: erro claro.
    let refused = move_surfaces("x\n1 0 -1 imp:p=1\n2 0 1 imp:p=0\n\n1 z 1 2 3 2\n\nm1 1001 1\n", &mcnp::parse("x\n1 0 -1 imp:p=1\n2 0 1 imp:p=0\n\n1 z 1 2 3 2\n\nm1 1001 1\n"), &[1], DVec3::X);
    assert!(refused.unwrap_err().contains("ao longo"));
}
