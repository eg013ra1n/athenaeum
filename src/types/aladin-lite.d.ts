declare module 'aladin-lite' {
  interface AladinView {
    gotoRaDec(ra: number, dec: number): void;
    setFoV(degrees: number): void;
    setRotation(degrees: number): void;
    getSize(): [number, number];
    world2pix(ra: number, dec: number): Float64Array | undefined;
  }
  interface AladinApi {
    init: Promise<void>;
    aladin(element: HTMLElement, options: Record<string, unknown>): AladinView;
    HiPS(url: string, options: Record<string, unknown>): unknown;
  }
  const A: AladinApi;
  export default A;
}
