class ModernMenuImage extends ModernMenuPage;

var localized string    SmoothingNames[5];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernAntiAliasing", 2, 4);
	for (i = 0; i < ArrayCount(SmoothingNames); i++)
		Combo.AddItem(SmoothingNames[i]);
	AddLevelSlider(1, "ModernBloom", 0, 10, 5);
	AddLevelSlider(2, "ModernSharpen", 0, 10, 4);
	AddBox(3, "ModernRelief");
	AddBox(4, "ModernSpecular");
	AddBack();
	LoadRows();
}

defaultproperties
{
	TitleText="Image and Effects"
	BackHelp="Return to the Modern Graphics menu."

	SmoothingNames(0)="Off"
	SmoothingNames(1)="FXAA"
	SmoothingNames(2)="SMAA"
	SmoothingNames(3)="MSAA 2x"
	SmoothingNames(4)="MSAA 4x"

	Labels(0)="Edge smoothing"
	Labels(1)="Bloom"
	Labels(2)="Sharpening"
	Labels(3)="Surface relief"
	Labels(4)="Generated specular"

	Helps(0)="Smooths jagged edges. FXAA is the fastest, SMAA looks better, MSAA costs the most."
	Helps(1)="A glow around bright light. 0 turns it off."
	Helps(2)="A crisper picture. 0 turns it off."
	Helps(3)="Bumps and depth on walls and floors, made from their own textures."
	Helps(4)="Shine on surfaces, made from their own textures."
}
