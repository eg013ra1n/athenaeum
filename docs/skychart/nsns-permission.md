# NSNS: the author's permission (phase 3 gate, #18)

The Northern Sky Narrowband Survey (NSNS) is by Stefan Ziegenbalg and licensed CC BY-NC-SA 4.0. Phase 3 of #18 was gated on his written agreement to its use in Athenaeum and to bulk downloads. This is his reply to the owner, recorded 2026-11-03, kept verbatim:

> Hi Vilen,
>
> 1. Data is can be used under terms of CC BY-NC-SA 4.0 license which covers your intended usage. If possible, please state the project name ("Northern Sky Narrowband Survey") and set a link (https://www.simg.de/nebulae3)
>
> 2. + 4. URLs should be obtained from the hipslist files of the HiPS server, see https://aladin.cds.unistra.fr/hips/list. (The GAVO server is not listed there because the GAVO guy believes that lists of HiPS servers should be retrieved from the VO, whereas the CDS guy thinks that servers should be reported to him manually 😉 ) If mirrors are known, it would be perfect if you implement some load balancing algorithm and use all of them.
>
> 3. Caching and/or downloading the data for offline use is permitted.
>
> Regards Stefan

## What it rules

| Point | Ruling for Athenaeum |
| ---- | ---- |
| 1. License | CC BY-NC-SA 4.0 covers the use. NSNS stays in the free Sky Chart, never in a paid (collaboration) surface. The credit names **"Northern Sky Narrowband Survey"** and links **https://www.simg.de/nebulae3**, the project page; the survey's own `obs_copyright_url` points at the DR0.2 page instead. |
| 2. + 4. URLs | Tile URLs come from HiPS servers' `hipslist` files, not from a URL written into the preset: the CDS list, the author's own `https://www.simg.de/hipslist`, and the GAVO server's list, which the CDS list does not carry. |
| 2. + 4. Mirrors | Every known mirror of a survey is used, with load balancing across them. `hips_status = public master clonable` allows copies from mirrors (unlike DSS colored's `clonableOnce`). |
| 3. Offline | The local tile store and the offline pack (phase 2) may hold NSNS tiles. |

The tiles are still never redistributed by Athenaeum (installer, Docker image, repo, artfrom.space, hub, Perseus): every install fetches its own. The design that follows from this ruling is on #18.
