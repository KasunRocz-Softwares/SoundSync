// KasunRocz Softwares — shared 3D hero planet for project sites.
// Usage: <canvas class="kr-planet" data-kind="ring|waves|crystal" data-color="#ffb547" aria-hidden="true"></canvas>
// The page works without it: if WebGL or the CDN fails, the canvas simply stays empty.
const canvas = document.querySelector('canvas.kr-planet');
const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;

if (canvas) {
  import('https://cdn.jsdelivr.net/npm/three@0.160.0/build/three.module.min.js')
    .then((THREE) => start(THREE))
    .catch((err) => console.warn('3D hero unavailable', err));
}

function start(THREE) {
  const small = innerWidth < 760 || matchMedia('(pointer: coarse)').matches;
  const kind = canvas.dataset.kind || 'ring';
  const color = new THREE.Color(canvas.dataset.color || '#3cf2c8');
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: !small, alpha: true, powerPreference: 'high-performance' });
  renderer.setPixelRatio(Math.min(devicePixelRatio || 1, small ? 1.5 : 1.75));
  renderer.setClearColor(0x000000, 0);
  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(40, 1, .1, 100);
  camera.position.set(0, 0, 11);

  const uTime = { value: 0 };
  const deep = color.clone().multiplyScalar(.14);
  const light = color.clone().lerp(new THREE.Color(0xffffff), .55);

  const c = document.createElement('canvas'); c.width = c.height = 64;
  const g = c.getContext('2d');
  const grd = g.createRadialGradient(32, 32, 0, 32, 32, 32);
  grd.addColorStop(0, 'rgba(255,255,255,1)'); grd.addColorStop(.25, 'rgba(255,255,255,.45)'); grd.addColorStop(1, 'rgba(255,255,255,0)');
  g.fillStyle = grd; g.fillRect(0, 0, 64, 64);
  const GLOW = new THREE.CanvasTexture(c);
  const glow = (col, size, opacity) => {
    const s = new THREE.Sprite(new THREE.SpriteMaterial({ map: GLOW, color: col, transparent: true, opacity, blending: THREE.AdditiveBlending, depthWrite: false }));
    s.scale.setScalar(size); return s;
  };
  const additive = (col, opacity, extra = {}) => new THREE.MeshBasicMaterial({ color: col, transparent: true, opacity, blending: THREE.AdditiveBlending, depthWrite: false, ...extra });

  const orb = (geo) => new THREE.Mesh(geo, new THREE.ShaderMaterial({
    uniforms: { uTime, uColor: { value: color }, uDeep: { value: deep } },
    vertexShader: `uniform float uTime; varying vec3 vN; varying vec3 vV; varying float vNoise;
      void main(){ vec3 p = position;
        float n = sin(p.x*2.3+uTime*.8)*sin(p.y*2.7+uTime*.6)*sin(p.z*2.1+uTime*.9);
        p += normal * n * .05; vNoise = n;
        vec4 mv = modelViewMatrix*vec4(p,1.); vN = normalize(normalMatrix*normal); vV = normalize(-mv.xyz);
        gl_Position = projectionMatrix*mv; }`,
    fragmentShader: `uniform vec3 uColor; uniform vec3 uDeep; varying vec3 vN; varying vec3 vV; varying float vNoise;
      void main(){ float f = 1. - clamp(dot(normalize(vN), normalize(vV)), 0., 1.);
        vec3 col = uDeep * (.15 + .3*(vNoise*.5+.5));
        col = mix(col, uColor, pow(f, 2.2)); col += uColor * pow(f, 6.) * 1.3;
        gl_FragColor = vec4(col, 1.); }`,
  }));

  const world = new THREE.Group();
  scene.add(world);
  const updates = [];

  // dust ring of particles around any planet, for depth
  const dust = (n, rMin, rMax, tilt, col2) => {
    const pos = new Float32Array(n * 3), cols = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) {
      const a = Math.random() * Math.PI * 2, r = rMin + Math.random() * (rMax - rMin);
      pos.set([Math.cos(a) * r, (Math.random() - .5) * .3, Math.sin(a) * r], i * 3);
      const cc = Math.random() < .7 ? color : col2;
      cols.set([cc.r, cc.g, cc.b], i * 3);
    }
    const geo = new THREE.BufferGeometry();
    geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    geo.setAttribute('color', new THREE.BufferAttribute(cols, 3));
    const pts = new THREE.Points(geo, new THREE.PointsMaterial({ size: .05, vertexColors: true, transparent: true, opacity: .8, blending: THREE.AdditiveBlending, depthWrite: false }));
    const pivot = new THREE.Group();
    pivot.rotation.set(tilt, 0, .3);
    pivot.add(pts);
    world.add(pivot);
    updates.push((t) => { pts.rotation.y = t * .06; });
  };

  if (kind === 'ring') {
    world.add(orb(new THREE.IcosahedronGeometry(1.7, small ? 16 : 28)));
    const ring = new THREE.Mesh(new THREE.RingGeometry(2.4, 4, 160, 1), new THREE.ShaderMaterial({
      uniforms: { uTime, uColor: { value: color } },
      vertexShader: 'varying vec2 vP; void main(){ vP = position.xy; gl_Position = projectionMatrix*modelViewMatrix*vec4(position,1.); }',
      fragmentShader: `uniform vec3 uColor; uniform float uTime; varying vec2 vP;
        void main(){ float r = length(vP); float t = (r-2.4)/1.6; float bands = .5+.5*sin(t*42.);
          float a = smoothstep(0.,.1,t)*smoothstep(1.,.8,t)*(.25+.75*bands)*.65;
          a *= .65 + .35*sin(atan(vP.y,vP.x)*2. + uTime*.7);
          gl_FragColor = vec4(uColor, a); }`,
      transparent: true, side: THREE.DoubleSide, depthWrite: false, blending: THREE.AdditiveBlending,
    }));
    ring.rotation.set(-1.15, .25, .35);
    world.add(ring);
    dust(small ? 500 : 1100, 4.4, 6.2, -1.15 + Math.PI / 2, light);
    const moonPivot = new THREE.Group(); moonPivot.rotation.set(.4, 0, -.3);
    const moon = orb(new THREE.IcosahedronGeometry(.26, 3)); moon.position.x = 6.8;
    moonPivot.add(moon); world.add(moonPivot);
    updates.push((t) => { ring.rotation.z = .35 + t * .08; moonPivot.rotation.y = t * .4; });
  } else if (kind === 'waves') {
    world.add(orb(new THREE.IcosahedronGeometry(1.5, small ? 16 : 28)));
    const waveGeo = new THREE.RingGeometry(.985, 1, 128, 1);
    const waves = Array.from({ length: 5 }, () => {
      const m = new THREE.Mesh(waveGeo, additive(color, .5, { side: THREE.DoubleSide }));
      m.rotation.set(-1.2, .3, 0); world.add(m); return m;
    });
    const BARS = 56;
    const eq = new THREE.InstancedMesh(new THREE.BoxGeometry(.08, 1, .08), additive(color, .85), BARS);
    const eqPivot = new THREE.Group(); eqPivot.rotation.set(-1.2 + Math.PI / 2, .3, 0); eqPivot.add(eq); world.add(eqPivot);
    const d = new THREE.Object3D();
    dust(small ? 400 : 900, 5.6, 6.6, .2, light);
    updates.push((t) => {
      waves.forEach((w, i) => { const ph = (t * .32 + i / waves.length) % 1; w.scale.setScalar(1.7 + ph * 4.6); w.material.opacity = (1 - ph) * .55; });
      for (let i = 0; i < BARS; i++) {
        const a = i / BARS * Math.PI * 2;
        d.position.set(Math.cos(a) * 2.35, 0, Math.sin(a) * 2.35); d.rotation.set(0, -a, 0);
        d.scale.set(1, .3 + Math.abs(Math.sin(t * 3.1 + i * .7) * Math.sin(t * 1.7 + i * .23)) * 1.5, 1);
        d.updateMatrix(); eq.setMatrixAt(i, d.matrix);
      }
      eq.instanceMatrix.needsUpdate = true; eqPivot.rotation.z = t * .1;
    });
  } else {
    const crystal = orb(new THREE.OctahedronGeometry(1.8, 0));
    world.add(crystal);
    const cage = new THREE.Mesh(new THREE.DodecahedronGeometry(3, 0), additive(color, .22, { wireframe: true }));
    world.add(cage);
    const LOCKS = 14;
    const locks = new THREE.InstancedMesh(new THREE.BoxGeometry(.18, .18, .18), new THREE.MeshBasicMaterial({ color: light }), LOCKS);
    const lp = new THREE.Group(); lp.rotation.set(.5, 0, .25); lp.add(locks); world.add(lp);
    const d = new THREE.Object3D();
    dust(small ? 350 : 700, 5.2, 7.5, .35, light);
    updates.push((t) => {
      crystal.rotation.y = t * .35; crystal.rotation.x = Math.sin(t * .5) * .25;
      cage.rotation.y = -t * .12; cage.rotation.x = t * .07;
      for (let i = 0; i < LOCKS; i++) {
        const a = i / LOCKS * Math.PI * 2 + t * .3;
        d.position.set(Math.cos(a) * 4, Math.sin(t * 2 + i) * .08, Math.sin(a) * 4);
        d.rotation.set(t + i, t * .6, 0); d.updateMatrix(); locks.setMatrixAt(i, d.matrix);
      }
      locks.instanceMatrix.needsUpdate = true;
    });
  }
  world.add(glow(color, 11, .4));

  // stars
  {
    const n = small ? 300 : 600, pos = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) pos.set([(Math.random() - .5) * 50, (Math.random() - .5) * 30, -10 - Math.random() * 25], i * 3);
    const geo = new THREE.BufferGeometry(); geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    scene.add(new THREE.Points(geo, new THREE.PointsMaterial({ color: 0xcfd8ff, size: .07, transparent: true, opacity: .55, depthWrite: false })));
  }

  const resize = () => {
    const w = canvas.clientWidth, h = canvas.clientHeight;
    if (!w || !h) return;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    camera.position.z = w / h < 1 ? 15 : 11;
    camera.updateProjectionMatrix();
  };
  resize();
  addEventListener('resize', resize);

  const ptr = { x: 0, y: 0, sx: 0, sy: 0 };
  addEventListener('pointermove', (e) => { ptr.x = e.clientX / innerWidth - .5; ptr.y = e.clientY / innerHeight - .5; }, { passive: true });

  const clock = new THREE.Clock();
  let t = 0, visible = true, raf = 0;
  const frame = () => {
    const dt = Math.min(clock.getDelta(), .05);
    t += dt;
    uTime.value = t;
    ptr.sx += (ptr.x - ptr.sx) * .05; ptr.sy += (ptr.y - ptr.sy) * .05;
    camera.position.x = ptr.sx * 1.2; camera.position.y = -ptr.sy * .8;
    camera.lookAt(0, 0, 0);
    world.rotation.y = Math.sin(t * .2) * .2;
    updates.forEach((u) => u(t));
    renderer.render(scene, camera);
    raf = visible && !reduce ? requestAnimationFrame(frame) : 0;
  };
  new IntersectionObserver(([e]) => {
    visible = e.isIntersecting;
    if (visible && !raf && !reduce) { clock.getDelta(); raf = requestAnimationFrame(frame); }
  }).observe(canvas);
  frame();
  requestAnimationFrame(() => canvas.classList.add('is-ready'));
}
