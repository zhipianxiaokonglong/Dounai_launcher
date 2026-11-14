/**
 * 一键启停：模块页与概览页共用同一份实现。
 *
 * 这两个动作原来只写在模块页。概览页也要同样的按钮，若各写一份，
 * 「提交了几步 / 哪一步失败」的措辞迟早分叉，同一个动作会给出两种结论。
 * 这里只管动作与结果，页面刷新由调用方决定。
 */

import { startAll, stopAll } from "../bridge.js";
import { toast } from "./dom.js";
import { describeError } from "./store.js";
import { formatChainResult, setLastChainRun } from "./views/chains.js";

/**
 * 同一时刻只允许一轮。这类操作在后端按拓扑串行，重复触发没有意义，
 * 而且两次点击会让结果 toast 互相覆盖。
 */
let busy = false;

async function runBulk(label: string, run: () => Promise<void>): Promise<void> {
  if (busy) {
    toast(`${label} 正在执行，请稍候…`);
    return;
  }
  busy = true;
  try {
    await run();
  } finally {
    busy = false;
  }
}

/** 一键启动 = 启动隐式默认链路。结果同时留在「链路」页。 */
export async function startEverything(): Promise<void> {
  await runBulk("一键启动", async () => {
    try {
      toast("正在按默认链路批量提交启动…");
      const result = await startAll();
      setLastChainRun(result);
      const summary = formatChainResult(result);
      const problems = result.steps.filter(
        (step) => step.state === "failed" || step.state === "blocked" || step.state === "dangling",
      );
      const submitted = summary.started + summary.external;
      if (problems.length === 0) {
        const tail = summary.external > 0 ? `，其中 ${summary.external} 个是外部服务` : "";
        toast(`默认链路已提交 ${submitted} / ${summary.total} 步${tail}；步骤见「链路」页`, "ok");
      } else {
        toast(
          `默认链路提交 ${submitted} / ${summary.total} 步，需注意：${problems
            .map((step) => `第 ${step.index} 步 ${step.instance_id}（${step.reason ?? "未说明"}）`)
            .join("；")}`,
          "error",
        );
      }
    } catch (error) {
      toast(`一键启动失败：${describeError(error)}`, "error");
    }
  });
}

/** 停止全部托管实例：逆拓扑、全局，不区分链路。 */
export async function stopEverything(): Promise<void> {
  await runBulk("一键停止", async () => {
    try {
      await stopAll();
      toast("已停止全部托管实例", "ok");
    } catch (error) {
      toast(`停止失败：${describeError(error)}`, "error");
    }
  });
}
