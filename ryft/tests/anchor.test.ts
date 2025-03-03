// No imports needed: web3, anchor, pg and more are globally available

describe("RYFT Tests", () => {
  let globalStateKp: web3.Keypair;
  let poolAccount: web3.Keypair;
  let liquidityProvider: web3.Keypair;
  let stakeVault: web3.Keypair;
  let borrower: web3.Keypair;
  let splToken: typeof import("@solana/spl-token");

  before(async () => {
    globalStateKp = new web3.Keypair();
    poolAccount = new web3.Keypair();
    liquidityProvider = new web3.Keypair();
    stakeVault = new web3.Keypair();
    borrower = new web3.Keypair();

    // Import SPL Token dynamically to avoid module errors
    splToken = await import("@solana/spl-token");
  });

  it("Initialize RYFT", async () => {
    const feeRate = new BN(500);

    const txHash = await pg.program.methods
      .initialize(feeRate)
      .accounts({
        globalState: globalStateKp.publicKey,
        admin: pg.wallet.publicKey,
        treasury: pg.wallet.publicKey,
        systemProgram: web3.SystemProgram.programId,
      })
      .signers([globalStateKp])
      .rpc();

    console.log(`Initialize TX: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);

    const globalState = await pg.program.account.globalState.fetch(
      globalStateKp.publicKey
    );

    console.log("Global State:", globalState);
    assert(globalState.feeRate.eq(feeRate));
  });

  it("Deposit Liquidity", async () => {
    const depositAmount = new BN(1000);

    const txHash = await pg.program.methods
      .depositLiquidity(depositAmount)
      .accounts({
        globalState: globalStateKp.publicKey,
        provider: liquidityProvider.publicKey,
        providerTokenAccount: pg.wallet.publicKey,
        poolAccount: poolAccount.publicKey,
        tokenProgram: splToken.TOKEN_PROGRAM_ID,
      })
      .signers([liquidityProvider])
      .rpc();

    console.log(`Liquidity Deposit TX: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);
  });

  it("Stake RYFT Tokens", async () => {
    const stakeAmount = new BN(500);

    const txHash = await pg.program.methods
      .stake(stakeAmount)
      .accounts({
        globalState: globalStateKp.publicKey,
        user: pg.wallet.publicKey,
        userTokenAccount: pg.wallet.publicKey,
        stakeVault: stakeVault.publicKey,
        stakeVaultAuthority: pg.wallet.publicKey,
        tokenProgram: splToken.TOKEN_PROGRAM_ID,
        systemProgram: web3.SystemProgram.programId,
      })
      .rpc();

    console.log(`Stake TX: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);
  });

  it("Flash Loan Execution", async () => {
    const loanAmount = new BN(200);
    const collateralAmount = new BN(100);
    const flashLoanStateKp = new web3.Keypair();
    const collateralEscrowKp = new web3.Keypair();

    const txHash = await pg.program.methods
      .flashLoan(loanAmount, collateralAmount)
      .accounts({
        globalState: globalStateKp.publicKey,
        poolAccount: poolAccount.publicKey,
        poolAuthority: pg.wallet.publicKey,
        borrowerTokenAccount: pg.wallet.publicKey,
        borrower: borrower.publicKey,
        flashLoanState: flashLoanStateKp.publicKey,
        borrowerCollateralAccount: pg.wallet.publicKey,
        collateralEscrow: collateralEscrowKp.publicKey,
        tokenProgram: splToken.TOKEN_PROGRAM_ID,
        systemProgram: web3.SystemProgram.programId,
      })
      .signers([borrower, flashLoanStateKp, collateralEscrowKp])
      .rpc();

    console.log(`Flash Loan TX: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);
  });

  it("Repay Flash Loan", async () => {
    const borrowerReputationKp = new web3.Keypair();
    const flashLoanStateKp = new web3.Keypair();

    const txHash = await pg.program.methods
      .repayFlashLoan()
      .accounts({
        globalState: globalStateKp.publicKey,
        poolAccount: poolAccount.publicKey,
        poolAuthority: pg.wallet.publicKey,
        flashLoanState: flashLoanStateKp.publicKey,
        borrower: borrower.publicKey,
        borrowerReputation: borrowerReputationKp.publicKey,
        tokenProgram: splToken.TOKEN_PROGRAM_ID,
        systemProgram: web3.SystemProgram.programId,
      })
      .signers([borrower, borrowerReputationKp])
      .rpc();

    console.log(`Repay Flash Loan TX: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);
  });
});
