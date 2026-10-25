/* FAKE nvml.dll v3 for the wine smoke test only (never shipped). Values drift with time
   so the 60 s graphs have something to draw. Two GPUs to exercise the picker.
   v3: if C:\\rmfake\\procs.txt exists, the GPU 0 process list comes from it, one process per line:
   "<pid> <vram_mb or -1> <G|C> <sm> <enc> <dec>" (real wine PIDs, so Toolhelp can resolve them). */
#include <stdio.h>
#include <windows.h>
#include <math.h>
#include <string.h>
typedef struct { unsigned gpu, memory; } U;
typedef struct { unsigned long long total, free, used; } M;
typedef struct { unsigned pid; unsigned long long used; unsigned gi, ci; } P2;
typedef struct { unsigned pid; unsigned long long ts; unsigned sm, mem, enc, dec; } PU;
#define EXP __declspec(dllexport)
static double T(void){ return GetTickCount64()/1000.0; }
static int idx(void*d){ return (int)(size_t)d - 1; }
EXP int nvmlInit_v2(void){return 0;}
EXP int nvmlShutdown(void){return 0;}
EXP int nvmlDeviceGetCount_v2(unsigned*n){*n=2;return 0;}
EXP int nvmlSystemGetDriverVersion(char*b,unsigned n){strncpy(b,"581.57",n);return 0;}
EXP int nvmlDeviceGetHandleByIndex_v2(unsigned i, void**d){ if(i>1) return 2; *d=(void*)(size_t)(i+1); return 0;}
EXP int nvmlDeviceGetName(void*d,char*b,unsigned n){strncpy(b, idx(d)==0?"FAKE GeForce RTX 5080":"FAKE GeForce RTX 3060", n);return 0;}
EXP int nvmlDeviceGetUtilizationRates(void*d,U*u){double t=T(); u->gpu=(unsigned)(79+6*sin(t/4)+3*sin(t*1.3)); u->memory=(unsigned)(41+8*sin(t/5)); return 0;}
EXP int nvmlDeviceGetTemperature(void*d,unsigned s,unsigned*t){*t=(unsigned)(78+3.5*sin(T()/9));return 0;}
EXP int nvmlDeviceGetMemoryInfo(void*d,M*m){m->total=16303ULL<<20;m->used=(unsigned long long)((11134+900*sin(T()/7))*1048576.0);m->free=m->total-m->used;return 0;}
EXP int nvmlDeviceGetPowerUsage(void*d,unsigned*p){*p=(unsigned)(300000+45000*sin(T()/3));return 0;}
EXP int nvmlDeviceGetEnforcedPowerLimit(void*d,unsigned*p){*p=360000;return 0;}
EXP int nvmlDeviceGetClockInfo(void*d,unsigned t,unsigned*c){*c= t==2?15001:(unsigned)(2640+60*sin(T()/2));return 0;}
EXP int nvmlDeviceGetMaxClockInfo(void*d,unsigned t,unsigned*c){*c= t==2?15001:3090;return 0;}
EXP int nvmlDeviceGetFanSpeed(void*d,unsigned*p){ if(idx(d)==1) return 3; *p=(unsigned)(62+5*sin(T()/6));return 0;}
EXP int nvmlDeviceGetEncoderUtilization(void*d,unsigned*u,unsigned*p){*u=(unsigned)(34+20*sin(T()/2.5)); *p=167000; return 0;}
EXP int nvmlDeviceGetDecoderUtilization(void*d,unsigned*u,unsigned*p){*u=(unsigned)(12+10*sin(T()/3.3)); *p=167000; return 0;}
EXP int nvmlDeviceGetPcieThroughput(void*d,unsigned c,unsigned*v){ Sleep(20); *v = c==1 ? (unsigned)(1800000+900000*sin(T()/2)) : (unsigned)(420000+200000*sin(T()/3)); return 0;}
EXP int nvmlDeviceGetCurrentClocksEventReasons(void*d,unsigned long long*m){
#ifdef FORCE_THR
  *m = 0x4 | 0x20; return 0;
#endif
  *m = (sin(T()/3) > 0.6) ? 0x4 : 0x0; if (sin(T()/9) > 0.9) *m |= 0x20; return 0;}
static const P2 GP[] = {{4412,0xFFFFFFFFFFFFFFFFULL,0,0},{1880,412ULL<<20,0,0},{9120,0xFFFFFFFFFFFFFFFFULL,0,0}};
static const P2 CP[] = {{34824,9216ULL<<20,0,0},{1880,0,0,0}};
typedef struct { unsigned pid; long long mb; char kind; unsigned sm, enc, dec; } FP;
static int fake(FP*o,int max){ FILE*f=fopen("C:\\rmfake\\procs.txt","r"); if(!f) return -1; int n=0;
  while(n<max && fscanf(f,"%u %lld %c %u %u %u",&o[n].pid,&o[n].mb,&o[n].kind,&o[n].sm,&o[n].enc,&o[n].dec)==6) n++;
  fclose(f); return n; }
static int flist(char kind,unsigned*c,P2*o){ FP f[16]; int n=fake(f,16); P2 t[16]; unsigned k=0;
  for(int i=0;i<n;i++) if(f[i].kind==kind){ t[k].pid=f[i].pid; t[k].used=f[i].mb<0?0xFFFFFFFFFFFFFFFFULL:(unsigned long long)f[i].mb<<20; t[k].gi=t[k].ci=0; k++; }
  if(*c<k){*c=k;return 7;} memcpy(o,t,k*sizeof(P2)); *c=k; return 0; }
static int list(const P2*src,unsigned n,unsigned*cnt,P2*out){ if(*cnt<n){*cnt=n;return 7;} memcpy(out,src,n*sizeof(P2)); *cnt=n; return 0; }
EXP int nvmlDeviceGetGraphicsRunningProcesses_v3(void*d,unsigned*c,P2*o){ if(idx(d)==1){*c=0;return 0;} FP f[1]; if(fake(f,1)>=0) return flist('G',c,o); return list(GP,3,c,o);}
EXP int nvmlDeviceGetComputeRunningProcesses_v3(void*d,unsigned*c,P2*o){ if(idx(d)==1){*c=0;return 0;} FP f[1]; if(fake(f,1)>=0) return flist('C',c,o); return list(CP,2,c,o);}
EXP int nvmlDeviceGetProcessUtilization(void*d,PU*u,unsigned*n,unsigned long long last){
  unsigned long long ts=(unsigned long long)(T()*1e6);
  FP f[16]; int k=fake(f,16);
  if(k>=0){ if(!u||*n<(unsigned)k){*n=k;return 7;}
    for(int i=0;i<k;i++){ double w=sin(T()/(2.0+i)); u[i].pid=f[i].pid; u[i].ts=ts; u[i].mem=1;
      u[i].sm=f[i].sm?(unsigned)(f[i].sm*(0.9+0.1*w)):0; u[i].enc=f[i].enc?(unsigned)(f[i].enc*(0.8+0.2*w)):0; u[i].dec=f[i].dec; }
    *n=k; return 0; }
  if(!u){*n=3;return 7;} if(*n<3){*n=3;return 7;}
  PU s[3]={{34824,ts,(unsigned)(71+5*sin(T())),30,0,0},{4412,ts,4,1,(unsigned)(34+20*sin(T()/2.5)),0},{1880,ts,2,1,0,0}};
  memcpy(u,s,sizeof s); *n=3; return 0; }
EXP int nvmlSystemGetProcessName(unsigned pid,char*b,unsigned n){
  const char*s = pid==34824?"C:\\Somewhere\\envs\\py\\python.exe": pid==4412?"C:\\Program Files\\OBS\\obs64.exe": pid==1880?"C:\\Windows\\System32\\dwm.exe":"";
  if(!*s) return 3; strncpy(b,s,n); return 0; }
