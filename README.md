# WasmITM

Calcul de propagation radio **ITM (Longley-Rice) v1.3** sur le MNE **Copernicus GLO-90**,
entièrement dans le navigateur (WebAssembly), sans serveur : un site statique déployable sur
GitHub Pages.

C'est le portage navigateur de [RustITM](https://github.com/loorisr/RustITM) (commit
`c3e7496`) :
- le modèle ITM (`crates/itm`) est repris tel quel ;
- la partie calcul du serveur (MNE, profils, couverture, mode rapide) est adaptée dans `crates/itm-wasm` ;
- l'interface web est la même.

Elle offre :
- des liaisons point à point avec profil de terrain ;
- des cartes de couverture sur la grille native GLO-90, en mode exact ou rapide (`fast`) ;
- l'export GeoTIFF et PNG, et l'import et l'export des sites.

## Fonctionnement

```
index.html / app.js     interface (Leaflet), identique à RustITM sauf l'appel de l'API
backend.js              routes /v1/info, /v1/elevation, /v1/p2p, /v1/coverage servies localement
worker.js               un Web Worker = une instance wasm avec ses propres tuiles
tiles.js                téléchargement des tuiles, cache du navigateur, import de fichiers
pkg/                    module wasm généré par build.sh (crates/itm-wasm)
```

- **Tuiles MNE.**
  - Le bucket AWS `copernicus-dem-90m`, utilisé par RustITM, n'autorise pas le CORS : un navigateur ne peut pas le lire.
  - Les tuiles viennent donc du miroir GLO-90 d'**OpenTopography** (`opentopography.s3.sdsc.edu/raster/COP90/COP90_hh/`), qui l'autorise.
  - Elles sont gardées dans le cache du navigateur (Cache API, environ 6 Mo par tuile). Un calcul de 100 km de rayon utilise 8 tuiles.
  - Une tuile absente du jeu Copernicus (mer) compte comme altitude 0 m.
  - L'onglet *Radio* permet d'importer des tuiles déjà téléchargées (AWS ou OpenTopography) et de vider le cache.
- **Parallélisme.**
  - Le calcul d'une couverture se fait en trois phases :
    1. un worker établit le plan : pour chaque pixel, le point polaire (radiale, pas) dont il prend la valeur ;
    2. tous les workers se partagent les radiales par blocs, à la demande : c'est la phase ITM, la plus coûteuse ;
    3. le premier worker reporte le résultat sur la grille.
  - Les threads WebAssembly ne sont pas utilisés : ils exigent les en-têtes COOP/COEP, que GitHub Pages ne permet pas d'envoyer.
  - Par défaut, la page lance `min(cœurs, 8)` workers. Le paramètre `?workers=N` dans l'URL change ce nombre.
  - Chaque worker garde sa propre copie des tuiles (environ 6 Mo par tuile), d'où ce plafond.
- **Mêmes résultats que RustITM.**
  - La validation des requêtes et les réponses JSON sont celles du serveur. `format` vaut `f32` ou `geotiff` ; `png` et `json` ne sont pas proposés.
  - Le calcul ne dépend pas du nombre de workers : le résultat est le même au bit près.

## Comparaison avec le serveur natif

Couverture de 100 km autour de 46,0° N 6,0° E (145 MHz, mât de 30 m, RX 2 m, grille native
3109 × 2161 px, 4,49 M appels ITM, mode `nearest`). Mêmes tuiles OpenTopography des deux côtés.

**Accuracy against the `itm-server` (RustITM) output:**
- **Coverage:** 15 of the 6.7 M pixels differ, by at most 1.5·10⁻⁵ dB (the last bit of an `f32`).
- **Point-to-point:** values differ at about 10⁻¹³ relative.
- **Cause:** the trigonometric functions of the WebAssembly runtime do not always round the last bit exactly like the native system library.

**Temps** (machine de 20 threads au repos, meilleur de 3 essais) :

| Calcul | Exact | `fast: 50` |
|---|---:|---:|
| RustITM natif, 1 thread | 12,4 s | 4,3 s |
| wasm, 1 worker (Node) | 16,7 s | 7,3 s |
| RustITM natif, 20 threads | 0,76 s | 0,28 s |
| Chromium, 20 workers (`?workers=20`) | 1,4 s | 0,9 s |
| Chromium, 8 workers (défaut) | 2,5 s | 1,3 s |

Comment lire ces temps :
- **Lignes navigateur** : mesurées une fois les tuiles en cache et les workers démarrés.
- **Premier calcul d'une page** : il faut ajouter le chargement des tuiles dans chaque worker, de 0,5 à 1,5 s, et leur téléchargement s'il n'a pas encore eu lieu. Pour 8 tuiles, environ 50 Mo.
- **Coût du wasm** : il ralentit ITM de 1,4× en mode exact et de 1,7× en mode rapide.
- **Mode rapide avec beaucoup de workers** : les phases non parallèles (plan et report, environ 0,4 s) pèsent dans le total.

## Compilation

Prérequis :
- Rust stable avec la cible `wasm32-unknown-unknown`
- `wasm-bindgen-cli`, à la même version que le crate : `cargo install --locked wasm-bindgen-cli --version 0.2.129`
- facultatif : `wasm-opt` (binaryen)

```sh
./build.sh                      # compile crates/itm-wasm et génère web/pkg/
cargo test --release            # tests du modèle ITM et du portage
cd web && python3 -m http.server 8000   # puis http://localhost:8000/
```

Un module ES chargé depuis `file://` ne fonctionne pas : il faut passer par un serveur HTTP local.

`scripts/node_run.mjs` exécute une requête de l'API dans Node (worker threads), à partir d'un
dossier de tuiles OpenTopography. Il sert aux comparaisons avec le serveur natif :

```sh
node scripts/node_run.mjs <tuiles> coverage requete.json sortie.f32 [workers]
node scripts/node_run.mjs <tuiles> p2p requete.json sortie.json
```

## Déploiement sur GitHub Pages

Le workflow `.github/workflows/pages.yml` lance les tests, compile le wasm et publie le dossier
`web/` à chaque push sur `main`. Pour l'activer :
1. pousser `main` sur GitHub ;
2. ouvrir *Settings → Pages → Build and deployment → Source* et choisir *GitHub Actions*.

Le site est alors servi à `https://loorisr.github.io/WasmITM/`. Tous les chemins sont
relatifs, donc il fonctionne aussi bien à la racine d'un domaine que dans un sous-dossier.

## Licence et données

- **Code** : AGPL-3.0 (voir `LICENSE`), comme RustITM. Leaflet est sous licence BSD-2 (`web/vendor/leaflet/LICENSE`).
- **MNE** : Copernicus GLO-90, © DLR e.V. 2010-2014 et © Airbus Defence and Space GmbH 2014-2018, fourni dans le cadre du programme Copernicus par l'ESA et l'Union européenne. Il est distribué ici via OpenTopography.
- **Fonds de carte** : © contributeurs OpenStreetMap, OpenTopoMap.
