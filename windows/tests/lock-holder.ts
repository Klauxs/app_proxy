import {Service} from '../src/service.ts';
const service=await new Service(process.argv[2]).init();
await service.store.lock(async()=>{
  console.log('LOCKED');
  await new Promise(()=>{setInterval(()=>{},1000);});
});
